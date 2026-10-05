use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};

pub type Route = [u8; 16];

pub fn derive(public: &[u8; 32]) -> Route {
    let mut digest = Sha256::new();
    digest.update(b"ahrara-route-v1\0");
    digest.update(public);
    digest.finalize()[..16].try_into().unwrap()
}

pub fn address(value: &str) -> Result<(Route, [u8; 8], String)> {
    let (local, domain) = value.split_once('@').context("invalid address")?;
    ensure!(
        local.len() == 32 && domain.eq_ignore_ascii_case("ahrara.dev"),
        "invalid address"
    );
    let raw = URL_SAFE_NO_PAD.decode(local)?;
    ensure!(
        raw.len() == 24 && URL_SAFE_NO_PAD.encode(&raw) == local,
        "invalid token"
    );

    Ok((
        raw[..16].try_into()?,
        raw[16..].try_into()?,
        format!("{local}@ahrara.dev"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_protocol_vector() {
        let vector: serde_json::Value =
            serde_json::from_str(include_str!("../../../tests/protocol-vectors.json")).unwrap();
        let public: [u8; 32] = hex::decode(vector["public_key"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();

        assert_eq!(
            hex::encode(derive(&public)),
            vector["route_id"].as_str().unwrap()
        );
        let (route, alias, _) = address(vector["address"].as_str().unwrap()).unwrap();

        assert_eq!(route, derive(&public));
        assert_eq!(hex::encode(alias), vector["alias_id"].as_str().unwrap());
    }

    #[test]
    fn address_roundtrip_and_canonical_domain() {
        let public = [42; 32];
        let route = derive(&public);
        let encoded = URL_SAFE_NO_PAD.encode([route.as_slice(), &[7; 8]].concat());
        let (got, alias, canonical) = address(&format!("{encoded}@AHRARA.DEV")).unwrap();

        assert_eq!(got, route);
        assert_eq!(alias, [7; 8]);
        assert_eq!(canonical, format!("{encoded}@ahrara.dev"));

        for bad in [
            "a@ahrara.dev",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA@example.test",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=@ahrara.dev",
        ] {
            assert!(address(bad).is_err());
        }
    }
}
