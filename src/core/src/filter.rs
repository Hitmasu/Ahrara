//! Shared predicates for inbox admission and individual receive operations.
use anyhow::{Result, bail};
use mailparse::MailHeaderMap;
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Every populated field must match. Text/header predicates are Unicode
/// case-insensitive substrings; envelope fields and Message-ID use exact matches.
/// Enable `regex` to interpret all string conditions as regular expressions.
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct EmailFilter {
    /// Interpret string conditions as case-insensitive Rust regex patterns.
    /// Text/header patterns search; `recipient`, `envelope_sender` and
    /// `message_id` must match the whole value, as if wrapped in `^(?:...)$`.
    /// Header names remain literal; inline flags can override case sensitivity.
    pub regex: bool,
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

pub(crate) struct CompiledEmailFilter {
    filter: EmailFilter,
    patterns: BTreeMap<String, Regex>,
    // Anchored separately: the same text may also be a search condition.
    whole: BTreeMap<String, Regex>,
}

impl EmailFilter {
    /// Validates limits before creating an inbox or starting a receive.
    pub fn validate(&self) -> Result<()> {
        self.compile().map(|_| ())
    }

    pub(crate) fn compile(&self) -> Result<CompiledEmailFilter> {
        let (mut patterns, mut whole_patterns) = (BTreeMap::new(), BTreeMap::new());
        let whole = [&self.recipient, &self.envelope_sender, &self.message_id];
        let search = [&self.sender, &self.subject, &self.to, &self.cc, &self.text];
        for (value, exact) in whole
            .into_iter()
            .flatten()
            .map(|value| (value, true))
            .chain(search.into_iter().flatten().map(|value| (value, false)))
            .chain(self.headers.values().map(|value| (value, false)))
        {
            if value.is_empty() || value.len() > 4096 || value.contains('\0') {
                bail!("invalid_argument: filter values must contain 1..4096 bytes without NUL")
            }
            let compiled = if exact {
                &mut whole_patterns
            } else {
                &mut patterns
            };
            if self.regex && !compiled.contains_key(value) {
                let build = |pattern: &str| {
                    RegexBuilder::new(pattern)
                        .case_insensitive(true)
                        .build()
                        .map_err(|_| {
                            anyhow::anyhow!("invalid_argument: invalid filter regular expression")
                        })
                };
                // Validate alone first: an unbalanced group could escape the anchors.
                let mut pattern = build(value)?;
                if exact {
                    pattern = build(&format!("^(?:{value})$"))?;
                }

                compiled.insert(value.clone(), pattern);
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

        Ok(CompiledEmailFilter {
            filter: self.clone(),
            patterns,
            whole: whole_patterns,
        })
    }
}

impl CompiledEmailFilter {
    pub(crate) fn matches(&self, raw: &[u8], recipient: &str, sender: &str) -> Result<bool> {
        let filter = &self.filter;
        if filter
            .recipient
            .as_ref()
            .is_some_and(|value| !self.value_matches(recipient, value, true))
            || filter
                .envelope_sender
                .as_ref()
                .is_some_and(|value| !self.value_matches(sender, value, true))
        {
            return Ok(false);
        }

        let parsed = mailparse::parse_mail(raw)
            .map_err(|_| anyhow::anyhow!("invalid_message: cannot evaluate email filter"))?;

        for (name, expected) in [
            ("From", &filter.sender),
            ("Subject", &filter.subject),
            ("To", &filter.to),
            ("Cc", &filter.cc),
        ] {
            if let Some(expected) = expected
                && !self.header_matches(&parsed.headers, name, expected)
            {
                return Ok(false);
            }
        }
        if filter.message_id.as_ref().is_some_and(|expected| {
            !parsed
                .headers
                .get_all_values("Message-ID")
                .iter()
                .any(|value| self.value_matches(value.trim(), expected, true))
        }) {
            return Ok(false);
        }
        if filter
            .headers
            .iter()
            .any(|(name, value)| !self.header_matches(&parsed.headers, name, value))
        {
            return Ok(false);
        }
        if let Some(expected) = filter.has_attachments
            && parsed.parts().any(crate::mail::is_attachment) != expected
        {
            return Ok(false);
        }
        if let Some(expected) = &filter.text {
            let (mut text, mut html) = (None, None);
            crate::mail::message_bodies(&parsed, &mut text, &mut html)?;
            if !text.is_some_and(|value| self.value_matches(&value, expected, false)) {
                return Ok(false);
            }
        }

        Ok(true)
    }

    fn value_matches(&self, value: &str, expected: &str, exact: bool) -> bool {
        if self.filter.regex {
            let patterns = if exact { &self.whole } else { &self.patterns };
            patterns[expected].is_match(value)
        } else if exact {
            value.eq_ignore_ascii_case(expected)
        } else {
            value.to_lowercase().contains(&expected.to_lowercase())
        }
    }

    fn header_matches(
        &self,
        headers: &[mailparse::MailHeader<'_>],
        name: &str,
        expected: &str,
    ) -> bool {
        headers
            .get_all_values(name)
            .iter()
            .any(|value| self.value_matches(value, expected, false))
    }
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
                .compile()
                .unwrap()
                .matches(raw, "inbox@example.test", "bounce@example.test")
                .unwrap()
        );
        filter.envelope_sender = Some("sender@example.test".into());

        assert!(
            !filter
                .compile()
                .unwrap()
                .matches(raw, "inbox@example.test", "bounce@example.test")
                .unwrap()
        );
        filter.envelope_sender = None;
        filter.message_id = Some("id".into());

        assert!(
            !filter
                .compile()
                .unwrap()
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

    #[test]
    fn regex_filters_match_decoded_fields_and_combine_conditions() {
        let raw = b"From: Sender <sender@example.test>\r\nTo: Inbox <inbox@example.test>\r\nCc: audit@example.test\r\nSubject: =?UTF-8?Q?Ol=C3=A1_123?=\r\nX-Trace: other\r\nX-Trace: flow-42\r\nMessage-ID: <id-123@example.test>\r\n\r\nCode 123\r\nDone";
        let filter: EmailFilter = serde_json::from_value(serde_json::json!({
            "regex": true,
            "sender": "^sender <sender@example\\.test>$",
            "subject": "^OLÁ [0-9]{3}$",
            "to": "<inbox@example\\.test>$",
            "cc": "^audit@",
            "recipient": "^INBOX@example\\.test$",
            "envelope_sender": "^bounce@(?:example|other)\\.test$",
            "message_id": "^<id-\\d+@example\\.test>$",
            "text": "(?m)^code \\d+\\r?$",
            "has_attachments": false,
            "headers": { "x-trace": "^FLOW-\\d+$" }
        }))
        .unwrap();

        filter.validate().unwrap();
        assert!(
            filter
                .compile()
                .unwrap()
                .matches(raw, "inbox@example.test", "bounce@example.test")
                .unwrap()
        );
        assert!(
            !filter
                .compile()
                .unwrap()
                .matches(raw, "another@example.test", "bounce@example.test")
                .unwrap()
        );
        let sensitive: EmailFilter = serde_json::from_value(serde_json::json!({
            "regex": true, "subject": "(?-i)^OLÁ"
        }))
        .unwrap();
        assert!(
            !sensitive
                .compile()
                .unwrap()
                .matches(raw, "inbox@example.test", "bounce@example.test")
                .unwrap()
        );
    }

    #[test]
    fn regex_envelope_and_message_id_conditions_match_whole_values() {
        let raw = b"From: sender@example.test\r\nMessage-ID: <id-123@example.test>\r\n\r\nbody";
        let matches = |conditions: serde_json::Value| {
            let mut conditions = conditions;
            conditions["regex"] = true.into();
            serde_json::from_value::<EmailFilter>(conditions)
                .unwrap()
                .compile()
                .unwrap()
                .matches(raw, "inbox@example.test", "bounce@example.test")
                .unwrap()
        };

        for (field, partial, whole) in [
            ("recipient", "inbox", r"INBOX@example\.test"),
            ("envelope_sender", r"example\.test", r"bounce@.+"),
            ("message_id", "id-123", r"<id-\d+@example\.test>"),
        ] {
            assert!(!matches(serde_json::json!({ field: partial })), "{field}");
            assert!(matches(serde_json::json!({ field: whole })), "{field}");
        }

        assert!(!matches(serde_json::json!({"recipient": "x|inbox"})));
        assert!(matches(
            serde_json::json!({"recipient": r"x|inbox@example\.test"})
        ));
        // The same pattern stays a substring search in other fields.
        assert!(!matches(
            serde_json::json!({"sender": "example", "envelope_sender": "example"})
        ));
        assert!(matches(
            serde_json::json!({"sender": "example", "envelope_sender": r"bounce@example\.test"})
        ));
        // An unbalanced group cannot escape the whole-value anchors.
        let escape: EmailFilter =
            serde_json::from_value(serde_json::json!({"regex": true, "recipient": "x)|(inbox"}))
                .unwrap();

        assert!(escape.validate().is_err());
    }

    #[test]
    fn literal_filters_preserve_metacharacters_and_exact_envelope_matching() {
        let raw = b"From: sender@example.test\r\nSubject: Receipt [123]. *?\r\n\r\nCode (123)";
        let mut filter = EmailFilter {
            subject: Some("[123]. *?".into()),
            text: Some("(123)".into()),
            ..Default::default()
        };
        assert!(
            filter
                .compile()
                .unwrap()
                .matches(raw, "inbox@example.test", "sender@example.test")
                .unwrap()
        );
        filter.recipient = Some("@example.test".into());
        assert!(
            !filter
                .compile()
                .unwrap()
                .matches(raw, "inbox@example.test", "sender@example.test")
                .unwrap()
        );
    }

    #[test]
    fn invalid_regex_is_rejected_in_every_field_without_echoing_patterns() {
        for field in [
            "sender",
            "subject",
            "to",
            "cc",
            "recipient",
            "envelope_sender",
            "message_id",
            "text",
            "headers",
        ] {
            for pattern in ["[private", "(?=private)", r"(private)\1"] {
                let value = if field == "headers" {
                    serde_json::json!({ "X-Trace": pattern })
                } else {
                    serde_json::json!(pattern)
                };
                let filter: EmailFilter =
                    serde_json::from_value(serde_json::json!({ "regex": true, field: value }))
                        .unwrap();
                let error = filter.validate().unwrap_err().to_string();
                assert!(error.starts_with("invalid_argument:"));
                assert!(!error.contains("private"));
            }
        }
    }

    #[tokio::test]
    async fn invalid_regex_fails_before_inbox_setup() {
        let error = crate::Inbox::create(crate::InboxOptions {
            filter: Some(EmailFilter {
                regex: true,
                subject: Some("[private".into()),
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .err()
        .expect("reject the pattern before requiring relay settings or opening a profile");
        assert_eq!(
            error.to_string(),
            "invalid_argument: invalid filter regular expression"
        );
    }
}
