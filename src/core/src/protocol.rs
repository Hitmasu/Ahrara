use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use data_encoding::BASE32_NOPAD;
use serde::Serialize;
use sha2::{Digest, Sha256};

pub const MAX_EMAIL: usize = 10 * 1024 * 1024;
pub const CHUNK_SIZE: usize = 64 * 1024;
pub const MAX_CONTROL: usize = 16 * 1024;
pub const MAX_FRAME: usize = 6 + 20 + CHUNK_SIZE;
pub const HELLO: u8 = 1;
pub const CHALLENGE: u8 = 2;
pub const AUTHENTICATE: u8 = 3;
pub const READY: u8 = 4;
pub const FAILURE: u8 = 5;
pub const OFFER: u8 = 10;
pub const ACCEPT: u8 = 11;
pub const REJECT: u8 = 12;
pub const START: u8 = 20;
pub const CHUNK: u8 = 21;
pub const END: u8 = 22;
pub const ABORT: u8 = 23;
pub const ACK: u8 = 24;
pub const NACK: u8 = 25;
/// Empty liveness frame, echoed by the relay: browsers cannot send WebSocket PINGs.
pub const KEEPALIVE: u8 = 30;

pub fn encode(kind: u8, payload: &[u8]) -> Result<Vec<u8>> {
    let limit = if kind == CHUNK {
        20 + CHUNK_SIZE
    } else {
        MAX_CONTROL
    };
    if payload.len() > limit {
        bail!("protocol frame too large")
    }

    let mut frame = Vec::with_capacity(6 + payload.len());
    frame.extend([1, kind]);
    frame.extend((payload.len() as u32).to_be_bytes());
    frame.extend(payload);
    Ok(frame)
}

pub fn json(kind: u8, value: &impl Serialize) -> Result<Vec<u8>> {
    encode(kind, &serde_json::to_vec(value)?)
}

pub fn decode(frame: &[u8]) -> Result<(u8, &[u8])> {
    if frame.len() < 6
        || frame[0] != 1
        || u32::from_be_bytes(frame[2..6].try_into()?) as usize != frame.len() - 6
    {
        bail!("invalid protocol frame")
    }

    let limit = if frame[1] == CHUNK {
        20 + CHUNK_SIZE
    } else {
        MAX_CONTROL
    };
    if frame.len() - 6 > limit {
        bail!("protocol frame too large")
    }

    Ok((frame[1], &frame[6..]))
}

/// Clients refuse a harder proof of work: it could not finish within the
/// authentication timeout on slow machines.
pub const MAX_WORK: u8 = 24;

/// Leading zero bits of the proof of work over one authentication challenge.
pub fn work(challenge: &[u8; 32], nonce: u64) -> u32 {
    let digest = Sha256::new()
        .chain_update(b"ahrara-work-v1\0")
        .chain_update(challenge)
        .chain_update(nonce.to_be_bytes())
        .finalize();
    u128::from_be_bytes(digest[..16].try_into().expect("16 bytes")).leading_zeros()
}

/// Finds a nonce with `bits` leading zero bits: about 2^bits SHA-256 hashes.
pub fn solve_work(challenge: &[u8; 32], bits: u8) -> u64 {
    (0..)
        .find(|&nonce| work(challenge, nonce) >= u32::from(bits))
        .expect("a nonce exists")
}

pub type Route = [u8; 16];

pub fn derive_route(public: &[u8; 32]) -> Route {
    let mut h = Sha256::new();
    h.update(b"ahrara-route-v1\0");
    h.update(public);
    h.finalize()[..16].try_into().expect("16 bytes")
}

/// New addresses are lowercase base32, so senders that lowercase addresses still reach them.
pub fn address(route: &Route, alias: &[u8; 8]) -> String {
    let token = BASE32_NOPAD.encode(&[route.as_slice(), alias].concat());
    format!("{}@ahrara.dev", token.to_ascii_lowercase())
}

/// Accepts base32 in any case and legacy 32-character base64url, whose case is significant.
pub fn decode_address(value: &str) -> Result<(Route, [u8; 8], String)> {
    let (local, domain) = value.split_once('@').context("invalid address")?;
    if !domain.eq_ignore_ascii_case("ahrara.dev") {
        bail!("invalid address")
    }

    let (bytes, canonical) = match local.len() {
        39 => {
            let canonical = local.to_ascii_lowercase();
            let bytes = BASE32_NOPAD
                .decode(local.to_ascii_uppercase().as_bytes())
                .context("invalid address token")?;
            ensure!(
                BASE32_NOPAD.encode(&bytes).to_ascii_lowercase() == canonical,
                "invalid address token"
            );
            (bytes, canonical)
        }
        32 => {
            let bytes = URL_SAFE_NO_PAD
                .decode(local)
                .context("invalid address token")?;
            ensure!(
                URL_SAFE_NO_PAD.encode(&bytes) == local,
                "invalid address token"
            );
            (bytes, local.to_owned())
        }
        _ => bail!("invalid address"),
    };
    ensure!(bytes.len() == 24, "invalid address token");

    Ok((
        bytes[..16].try_into()?,
        bytes[16..].try_into()?,
        format!("{canonical}@ahrara.dev"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proof_of_work_is_bound_to_its_challenge() {
        let challenge = [7; 32];
        let nonce = solve_work(&challenge, 12);

        assert!(work(&challenge, nonce) >= 12);
        assert!((0..nonce).all(|earlier| work(&challenge, earlier) < 12));
        assert_eq!(solve_work(&challenge, 0), 0);
        assert!(
            work(&[8; 32], nonce) < 12,
            "work proven for another challenge"
        );
    }

    #[test]
    fn address_survives_case_changes() {
        let route = derive_route(&[42; 32]);
        let generated = address(&route, &[7; 8]);
        let (local, _) = generated.split_once('@').unwrap();

        assert_eq!(local.len(), 39);
        assert_eq!(generated, generated.to_ascii_lowercase());
        for variant in [generated.to_ascii_uppercase(), mixed_case(&generated)] {
            assert_eq!(
                decode_address(&variant).unwrap(),
                (route, [7; 8], generated.clone())
            );
        }

        let zeros = "a".repeat(38);
        for bad in [
            format!("{zeros}b@ahrara.dev"), // non-zero trailing bits
            format!("{zeros}1@ahrara.dev"),
            format!("{zeros}=@ahrara.dev"),
            format!("{zeros}a@example.test"),
        ] {
            assert!(decode_address(&bad).is_err(), "{bad}");
        }
        assert!(decode_address(&format!("{zeros}a@ahrara.dev")).is_ok());
    }

    fn mixed_case(value: &str) -> String {
        value
            .chars()
            .enumerate()
            .map(|(i, c)| {
                if i % 2 == 0 {
                    c.to_ascii_uppercase()
                } else {
                    c
                }
            })
            .collect()
    }

    #[test]
    fn legacy_address_roundtrip_and_canonical_domain() {
        let route = derive_route(&[42; 32]);
        let encoded = URL_SAFE_NO_PAD.encode([route.as_slice(), &[7; 8]].concat());
        let (got, alias, canonical) = decode_address(&format!("{encoded}@AHRARA.DEV")).unwrap();

        assert_eq!(got, route);
        assert_eq!(alias, [7; 8]);
        assert_eq!(canonical, format!("{encoded}@ahrara.dev"));

        for bad in [
            "a@ahrara.dev",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA@example.test",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=@ahrara.dev",
        ] {
            assert!(decode_address(bad).is_err());
        }
    }
}
