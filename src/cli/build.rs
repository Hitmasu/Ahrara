fn main() {
    println!("cargo:rerun-if-env-changed=AHRARA_SERVER_PUBLIC_KEY");
    if let Ok(key) = std::env::var("AHRARA_SERVER_PUBLIC_KEY") {
        assert!(
            key.is_empty()
                || key
                    .strip_prefix("ahrara_server_v1_")
                    .is_some_and(|pin| pin.len() == 43
                        && pin
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')),
            "AHRARA_SERVER_PUBLIC_KEY must contain the verified public relay transport key"
        );
    }

    println!("cargo:rerun-if-env-changed=AHRARA_UPDATE_PUBLIC_KEY");
    if let Ok(key) = std::env::var("AHRARA_UPDATE_PUBLIC_KEY") {
        assert!(
            key.is_empty() || (key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit())),
            "AHRARA_UPDATE_PUBLIC_KEY must contain the 32-byte Ed25519 release public key as hex"
        );
    }
}
