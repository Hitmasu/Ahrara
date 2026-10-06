use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
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

pub fn decode_address(value: &str) -> Result<(Route, [u8; 8], String)> {
    let (local, domain) = value.split_once('@').context("invalid address")?;
    if local.len() != 32 || !domain.eq_ignore_ascii_case("ahrara.dev") {
        bail!("invalid address")
    }

    let bytes = URL_SAFE_NO_PAD
        .decode(local)
        .context("invalid address token")?;
    if bytes.len() != 24 || URL_SAFE_NO_PAD.encode(&bytes) != local {
        bail!("invalid address token")
    }

    Ok((
        bytes[..16].try_into()?,
        bytes[16..].try_into()?,
        format!("{local}@ahrara.dev"),
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
    fn address_roundtrip_and_canonical_domain() {
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
