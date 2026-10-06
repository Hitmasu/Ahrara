use crate::protocol;
use anyhow::{Context, Result, ensure};
use ipnet::IpNet;
use mailparse::{MailAddr, MailHeaderMap};
use std::net::IpAddr;

pub const MAX_HEADERS: usize = 64 * 1024;

pub struct Config {
    prefixes: Vec<IpNet>,
    address: Option<String>,
    verification_recipient: Option<String>,
}

impl Config {
    pub fn new(sources: &str, address: &str, verification_recipient: &str) -> Result<Self> {
        let prefixes = prefixes(sources)?;
        let address = if address.is_empty() {
            None
        } else {
            let parsed = mailparse::addrparse(address)?;
            ensure!(
                !prefixes.is_empty()
                    && !address.contains(['\r', '\n'])
                    && parsed.len() == 1
                    && matches!(&parsed[0], MailAddr::Single(mailbox) if mailbox.addr == address),
                "Email Routing requires one sink address and explicit trusted sources"
            );
            Some(address.to_owned())
        };
        let verification_recipient = if verification_recipient.is_empty() {
            None
        } else {
            ensure!(
                address.is_some(),
                "verification requires an Email Routing sink"
            );
            Some(protocol::decode_address(verification_recipient)?.2)
        };
        Ok(Self {
            prefixes,
            address,
            verification_recipient,
        })
    }

    pub fn trusted(&self, peer: IpAddr) -> bool {
        trusted(peer, &self.prefixes)
    }

    /// With an Email Routing sink, only the provider may deliver: direct SMTP would
    /// bypass its filtering and could hold recipients reserved.
    pub fn refuses(&self, peer: IpAddr) -> bool {
        self.address.is_some() && !self.trusted(peer)
    }

    pub fn envelope(&self, value: &str) -> Result<String> {
        if let Some(address) = &self.address {
            ensure!(
                value.eq_ignore_ascii_case(address),
                "invalid forwarding envelope"
            );
            Ok(address.clone())
        } else {
            Ok(protocol::decode_address(value)?.2)
        }
    }

    pub fn recipient(&self, raw: &[u8]) -> Result<String> {
        let Some(sink) = &self.address else {
            return recipient(raw);
        };
        ensure!(raw.len() <= MAX_HEADERS, "routing headers too large");
        let (headers, consumed) = mailparse::parse_headers(raw)?;
        ensure!(consumed == raw.len(), "invalid routing header prefix");
        if let Some(recipient) = &self.verification_recipient {
            // Setup mail goes only to the operator's connected inbox. Provider
            // headers are inspected there before enabling recipient routing.
            return Ok(recipient.clone());
        }

        let destinations = headers.get_all_values("X-Forwarded-To");
        ensure!(
            destinations.len() == 1 && destinations[0].trim().eq_ignore_ascii_case(sink),
            "invalid forwarding sink"
        );
        let forwards = headers.get_all_values("X-Forwarded-For");
        ensure!(forwards.len() == 1, "ambiguous forwarding recipient");
        // Native Cloudflare forwarding supplies: original-recipient SMTP-sink.
        let mut addresses = forwards[0].split_ascii_whitespace();
        let original = addresses.next().context("missing forwarding recipient")?;
        ensure!(
            addresses
                .next()
                .is_some_and(|value| value.eq_ignore_ascii_case(sink))
                && addresses.next().is_none(),
            "invalid forwarding chain"
        );
        Ok(protocol::decode_address(original)?.2)
    }
}

pub fn prefixes(value: &str) -> Result<Vec<IpNet>> {
    if value.trim().is_empty() {
        return Ok(Vec::new());
    }

    value
        .split(',')
        .map(|part| {
            let prefix: IpNet = part
                .trim()
                .parse()
                .context("invalid AHRARA_TRUSTED_EMAIL_ROUTING_CIDRS")?;
            ensure!(
                prefix.prefix_len() > 0
                    && !matches!(prefix.addr(),IpAddr::V6(ip) if ip.to_ipv4_mapped().is_some()),
                "use explicit IPv4/IPv6 CIDRs, never a default route"
            );

            Ok(prefix)
        })
        .collect()
}

pub fn trusted(peer: IpAddr, prefixes: &[IpNet]) -> bool {
    let peer = match peer {
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(peer),
        _ => peer,
    };

    prefixes.iter().any(|prefix| prefix.contains(&peer))
}

pub fn recipient(raw: &[u8]) -> Result<String> {
    ensure!(raw.len() <= MAX_HEADERS, "routing headers too large");
    let (headers, consumed) = mailparse::parse_headers(raw)?;
    ensure!(consumed == raw.len(), "invalid routing header prefix");
    let mut recipient = None;

    for header in headers {
        if !header.get_key_ref().eq_ignore_ascii_case("X-Forwarded-To") {
            continue;
        }

        let value = header.get_value();
        let addresses = mailparse::addrparse(&value)?;
        ensure!(addresses.len() == 1, "ambiguous forwarding recipient");
        let MailAddr::Single(address) = &addresses[0] else {
            anyhow::bail!("ambiguous forwarding recipient")
        };
        let (_, _, canonical) = protocol::decode_address(&address.addr)?;
        ensure!(
            recipient
                .as_ref()
                .is_none_or(|previous| previous == &canonical),
            "conflicting forwarding recipient"
        );

        recipient = Some(canonical);
    }

    recipient.context("missing forwarding recipient")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_email_routing_preserves_original_recipient() {
        let sink = "relay@ingest.ahrara.dev";
        let inbox = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA@ahrara.dev";
        let config = Config::new("104.30.0.0/19", sink, "").unwrap();
        for forwarded in [format!("{inbox} {sink}"), format!("{inbox}\r\n\t{sink}")] {
            let raw = format!(
                "X-Forwarded-To: {sink}\r\nX-Forwarded-For: {forwarded}\r\nTo: unrelated@example.test\r\n\r\n"
            );
            assert_eq!(config.recipient(raw.as_bytes()).unwrap(), inbox);
        }
        for raw in [
            format!("X-Forwarded-To: {sink}\r\n\r\n"),
            format!("X-Forwarded-For: {inbox} {sink}\r\n\r\n"),
            format!(
                "X-Forwarded-To: other@example.test\r\nX-Forwarded-For: {inbox} {sink}\r\n\r\n"
            ),
            format!(
                "X-Forwarded-To: {sink}\r\nX-Forwarded-For: {inbox} other@example.test\r\n\r\n"
            ),
            format!(
                "X-Forwarded-To: {sink}\r\nX-Forwarded-For: {inbox} {sink} extra@example.test\r\n\r\n"
            ),
            format!(
                "X-Forwarded-To: {sink}\r\nX-Forwarded-For: invalid@example.test {sink}\r\n\r\n"
            ),
            format!(
                "X-Forwarded-To: {sink}\r\nX-Forwarded-To: {sink}\r\nX-Forwarded-For: {inbox} {sink}\r\n\r\n"
            ),
            format!(
                "X-Forwarded-To: {sink}\r\nX-Forwarded-For: {inbox} {sink}\r\nX-Forwarded-For: {inbox} {sink}\r\n\r\n"
            ),
            format!("To: {inbox}\r\nReceived: for <{inbox}>\r\n\r\n"),
        ] {
            assert!(config.recipient(raw.as_bytes()).is_err());
        }
    }

    #[test]
    fn email_routing_requires_trusted_sources_and_exact_sink() {
        let sink = "relay@ingest.ahrara.dev";
        let inbox = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA@ahrara.dev";
        let config = Config::new("104.30.0.0/19", sink, "").unwrap();
        assert!(config.trusted("104.30.0.7".parse().unwrap()));
        assert!(!config.trusted("127.0.0.1".parse().unwrap()));
        assert!(config.envelope(sink).is_ok());
        assert!(config.envelope(inbox).is_err());
        assert!(config.envelope("other@ingest.ahrara.dev").is_err());
        assert!(config.recipient(b"Subject: no route\r\n\r\n").is_err());
        let verification = Config::new("104.30.0.0/19", sink, inbox).unwrap();
        assert_eq!(
            verification.recipient(b"Subject: verify\r\n\r\n").unwrap(),
            inbox
        );
        let headers = format!("X-Forwarded-To: {sink}\r\n\r\n");
        assert_eq!(verification.recipient(headers.as_bytes()).unwrap(), inbox);
        assert!(
            verification
                .recipient(&vec![b'A'; MAX_HEADERS + 1])
                .is_err()
        );
        for (sources, address, fallback) in [
            ("", sink, ""),
            ("104.30.0.0/19", "", inbox),
            ("104.30.0.0/19", sink, "invalid@example.test"),
            ("104.30.0.0/19", "Relay <relay@ingest.ahrara.dev>", ""),
            ("104.30.0.0/19", "relay@ingest.ahrara.dev\r\n", ""),
        ] {
            assert!(Config::new(sources, address, fallback).is_err());
        }
    }

    #[test]
    fn trust_and_ambiguous_forwarding() {
        let nets = prefixes("104.30.0.0/19,2405:8100:c000::/38").unwrap();
        assert!(trusted("::ffff:104.30.0.7".parse().unwrap(), &nets));
        assert!(!trusted("127.0.0.1".parse().unwrap(), &nets));

        for value in [
            "invalid",
            "127.0.0.1",
            "0.0.0.0/0",
            "::/0",
            "127.0.0.1/32,",
            "::ffff:104.30.0.7/128",
        ] {
            assert!(prefixes(value).is_err());
        }

        let address = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA@ahrara.dev";

        for raw in [
            format!("X-Forwarded-To: {address}\r\n\r\n"),
            format!("X-Forwarded-To:\r\n\t<{address}>\r\n\r\n"),
            format!("X-Forwarded-To: {address}\r\nX-Forwarded-To: {address}\r\n\r\n"),
        ] {
            assert_eq!(recipient(raw.as_bytes()).unwrap(), address);
        }

        for raw in [
            "To: ignored\r\n\r\n".to_string(),
            format!("X-Forwarded-To: {address}, {address}\r\n\r\n"),
            format!(
                "X-Forwarded-To: {address}\r\nX-Forwarded-To: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB@ahrara.dev\r\n\r\n"
            ),
            "bad header\r\n\r\n".into(),
        ] {
            assert!(recipient(raw.as_bytes()).is_err());
        }
    }
}
