//! Shared predicates for inbox admission and individual receive operations.
use anyhow::{Result, bail};
use mailparse::MailHeaderMap;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Every populated field must match. Text/header predicates are Unicode
/// case-insensitive substrings; envelope fields and Message-ID use exact matches.
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct EmailFilter {
    /// Matches the decoded From header, including display names.
    pub sender: Option<String>,
    pub subject: Option<String>,
    pub to: Option<String>,
    pub cc: Option<String>,
    /// Matches the actual SMTP recipient, independently of the To header.
    pub recipient: Option<String>,
    pub envelope_sender: Option<String>,
    pub message_id: Option<String>,
    /// Searches the complete decoded plain-text body, excluding attachments.
    pub text: Option<String>,
    pub has_attachments: Option<bool>,
    /// Header names are case-insensitive. Any repeated value may match.
    pub headers: BTreeMap<String, String>,
}

impl EmailFilter {
    /// Validates limits before creating an inbox or starting a receive.
    pub fn validate(&self) -> Result<()> {
        for value in [
            &self.sender,
            &self.subject,
            &self.to,
            &self.cc,
            &self.recipient,
            &self.envelope_sender,
            &self.message_id,
            &self.text,
        ]
        .into_iter()
        .flatten()
        .chain(self.headers.values())
        {
            if value.is_empty() || value.len() > 4096 || value.contains('\0') {
                bail!("invalid_argument: filter values must contain 1..4096 bytes without NUL")
            }
        }
        if self.headers.len() > 64
            || self.headers.keys().any(|name| {
                name.is_empty()
                    || name.len() > 256
                    || !name
                        .bytes()
                        .all(|byte| (33..=126).contains(&byte) && byte != b':')
            })
        {
            bail!("invalid_argument: filter requires at most 64 valid header names")
        }

        Ok(())
    }

    pub(crate) fn matches(&self, raw: &[u8], recipient: &str, sender: &str) -> Result<bool> {
        if self
            .recipient
            .as_ref()
            .is_some_and(|value| !recipient.eq_ignore_ascii_case(value))
            || self
                .envelope_sender
                .as_ref()
                .is_some_and(|value| !sender.eq_ignore_ascii_case(value))
        {
            return Ok(false);
        }

        let parsed = mailparse::parse_mail(raw)
            .map_err(|_| anyhow::anyhow!("invalid_message: cannot evaluate email filter"))?;

        for (name, expected) in [
            ("From", &self.sender),
            ("Subject", &self.subject),
            ("To", &self.to),
            ("Cc", &self.cc),
        ] {
            if let Some(expected) = expected
                && !header_contains(&parsed.headers, name, expected)
            {
                return Ok(false);
            }
        }
        if self.message_id.as_ref().is_some_and(|expected| {
            !parsed
                .headers
                .get_all_values("Message-ID")
                .iter()
                .any(|value| value.trim().eq_ignore_ascii_case(expected))
        }) {
            return Ok(false);
        }
        if self
            .headers
            .iter()
            .any(|(name, value)| !header_contains(&parsed.headers, name, value))
        {
            return Ok(false);
        }
        if let Some(expected) = self.has_attachments
            && parsed.parts().any(crate::mail::is_attachment) != expected
        {
            return Ok(false);
        }
        if let Some(expected) = &self.text {
            let (mut text, mut html) = (None, None);
            crate::mail::message_bodies(&parsed, &mut text, &mut html)?;
            if !text.is_some_and(|value| value.to_lowercase().contains(&expected.to_lowercase())) {
                return Ok(false);
            }
        }

        Ok(true)
    }
}

fn header_contains(headers: &[mailparse::MailHeader<'_>], name: &str, expected: &str) -> bool {
    let expected = expected.to_lowercase();
    headers
        .get_all_values(name)
        .iter()
        .any(|value| value.to_lowercase().contains(&expected))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_are_combined_and_headers_are_decoded_and_repeated() {
        let raw = b"From: Sender <sender@example.test>\r\nSubject: =?UTF-8?Q?Ol=C3=A1?=\r\nX-Trace: first\r\nX-Trace: second\r\nMessage-ID: <id@example.test>\r\n\r\nToken 123";
        let mut filter = EmailFilter {
            sender: Some("SENDER".into()),
            subject: Some("OLÁ".into()),
            recipient: Some("inbox@example.test".into()),
            text: Some("token 123".into()),
            has_attachments: Some(false),
            headers: BTreeMap::from([("x-trace".into(), "SECOND".into())]),
            ..Default::default()
        };
        filter.validate().unwrap();

        assert!(
            filter
                .matches(raw, "inbox@example.test", "bounce@example.test")
                .unwrap()
        );
        filter.envelope_sender = Some("sender@example.test".into());

        assert!(
            !filter
                .matches(raw, "inbox@example.test", "bounce@example.test")
                .unwrap()
        );
        filter.envelope_sender = None;
        filter.message_id = Some("id".into());

        assert!(
            !filter
                .matches(raw, "inbox@example.test", "bounce@example.test")
                .unwrap()
        );
    }

    #[test]
    fn invalid_filter_configuration_is_rejected_without_echoing_input() {
        let filter = EmailFilter {
            headers: BTreeMap::from([("bad:secret".into(), "x".into())]),
            ..Default::default()
        };
        let error = filter.validate().unwrap_err().to_string();

        assert!(error.starts_with("invalid_argument:"));
        assert!(!error.contains("secret"));
    }
}
