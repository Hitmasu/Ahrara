use crate::auth;
use anyhow::{Context, Result, ensure};
use ipnet::IpNet;
use mailparse::MailAddr;
use std::net::IpAddr;

pub const MAX_HEADERS: usize = 64 * 1024;

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
        let (_, _, canonical) = auth::address(&address.addr)?;
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
