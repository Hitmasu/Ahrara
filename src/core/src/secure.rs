//! Authenticated transport between the Rust relay and client, inside WSS.
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use snow::{HandshakeState, TransportState, resolvers::DefaultResolver};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    path::Path,
    sync::{Arc, Mutex},
};
use zeroize::Zeroizing;

pub const PATTERN: &str = "Noise_NK_25519_ChaChaPoly_SHA256";
pub const PROLOGUE: &[u8] = b"ahrara-transport-v2\0";
pub const TAG_LEN: usize = 16;
pub const MAX_CIPHERTEXT: usize = 65_535;
pub const MAX_PLAINTEXT: usize = MAX_CIPHERTEXT - TAG_LEN;
pub const MAX_HANDSHAKE: usize = 256;

pub struct Sender(Arc<Mutex<TransportState>>);

pub struct Receiver(Arc<Mutex<TransportState>>);

fn builder<'a>() -> Result<snow::Builder<'a>> {
    // RustCrypto retains 32 bytes per cipher versus Ring's 544; natively Ring
    // supplies the RNG, while browsers have no Ring and use getrandom instead.
    #[cfg(not(target_arch = "wasm32"))]
    let resolver = Box::new(snow::resolvers::FallbackResolver::new(
        Box::new(DefaultResolver),
        Box::new(snow::resolvers::RingResolver),
    ));
    #[cfg(target_arch = "wasm32")]
    let resolver = Box::new(DefaultResolver);
    Ok(snow::Builder::with_resolver(PATTERN.parse()?, resolver))
}

pub fn initiator(remote: &[u8; 32]) -> Result<HandshakeState> {
    Ok(builder()?
        .prologue(PROLOGUE)?
        .remote_public_key(remote)?
        .build_initiator()?)
}

pub fn responder(secret: &[u8; 32]) -> Result<HandshakeState> {
    Ok(builder()?
        .prologue(PROLOGUE)?
        .local_private_key(secret)?
        .build_responder()?)
}

pub fn into_session(state: HandshakeState) -> Result<(Sender, Receiver)> {
    let state = Arc::new(Mutex::new(state.into_transport_mode()?));
    Ok((Sender(state.clone()), Receiver(state)))
}

impl Sender {
    pub fn seal(&mut self, plain: &[u8]) -> Result<Vec<u8>> {
        ensure!(plain.len() <= MAX_PLAINTEXT, "secure frame too large");
        let mut cipher = vec![0; plain.len() + TAG_LEN];
        let length = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("secure session unavailable"))?
            .write_message(plain, &mut cipher)?;
        cipher.truncate(length);
        Ok(cipher)
    }
}

impl Receiver {
    pub fn open(&mut self, cipher: &[u8]) -> Result<Vec<u8>> {
        ensure!(
            (TAG_LEN..=MAX_CIPHERTEXT).contains(&cipher.len()),
            "invalid secure frame length"
        );
        // Ring decrypts in place when the output also has room for the tag.
        let mut plain = Zeroizing::new(vec![0; cipher.len()]);
        let length = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("secure session unavailable"))?
            .read_message(cipher, &mut plain)?;
        plain.truncate(length);
        // Transfer ownership to the caller; a failed authentication wipes the scratch buffer.
        Ok(std::mem::take(&mut *plain))
    }
}

pub fn auth_preimage(public: &[u8; 32], challenge: &[u8; 32], binding: &[u8; 32]) -> Vec<u8> {
    let mut bytes = b"ahrara-auth-v2\0".to_vec();
    bytes.extend(public);
    bytes.extend(challenge);
    bytes.extend(binding);
    bytes
}

pub fn parse_public_key(value: &str) -> Result<[u8; 32]> {
    let encoded = value
        .strip_prefix("ahrara_server_v1_")
        .context("invalid server public key format")?;
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .context("invalid server public key encoding")?;
    ensure!(
        bytes.len() == 32 && URL_SAFE_NO_PAD.encode(&bytes) == encoded,
        "invalid server public key length or encoding"
    );

    Ok(bytes.as_slice().try_into()?)
}

pub fn load_key(path: &Path) -> Result<Zeroizing<[u8; 32]>> {
    let mut file = std::fs::File::open(path).context("cannot read transport private key")?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() == 32,
        "invalid transport private key file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "transport private key permissions must be private"
        );
    }
    let mut key = Zeroizing::new([0; 32]);
    file.read_exact(&mut *key)?;
    let mut extra = [0];
    ensure!(
        file.read(&mut extra)? == 0,
        "invalid transport private key length"
    );

    Ok(key)
}

pub fn keygen(path: &Path) -> Result<String> {
    let key = builder()?.generate_keypair()?;
    let private = Zeroizing::new(key.private);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .context("cannot create transport private key")?;
    file.write_all(&private)?;
    file.sync_all()?;
    Ok(format!(
        "ahrara_server_v1_{}",
        URL_SAFE_NO_PAD.encode(key.public)
    ))
}

pub fn public_key(path: &Path) -> Result<String> {
    use snow::resolvers::CryptoResolver;
    let private = load_key(path)?;
    let mut dh = snow::resolvers::DefaultResolver
        .resolve_dh(&snow::params::DHChoice::Curve25519)
        .context("transport key primitive unavailable")?;
    dh.set(&*private);
    Ok(format!(
        "ahrara_server_v1_{}",
        URL_SAFE_NO_PAD.encode(dh.pubkey())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

    fn handshake(pin: Option<[u8; 32]>) -> Result<(HandshakeState, HandshakeState)> {
        let key = snow::Builder::new(PATTERN.parse()?).generate_keypair()?;
        let secret: [u8; 32] = key.private.as_slice().try_into()?;
        let public: [u8; 32] = key.public.as_slice().try_into()?;
        let mut client = initiator(&pin.unwrap_or(public))?;
        let mut server = responder(&secret)?;
        let mut message = [0; MAX_HANDSHAKE];
        let mut payload = [0; MAX_HANDSHAKE];
        let length = client.write_message(&[], &mut message)?;

        assert_eq!(server.read_message(&message[..length], &mut payload)?, 0);
        let length = server.write_message(&[], &mut message)?;

        assert_eq!(client.read_message(&message[..length], &mut payload)?, 0);
        assert_eq!(client.get_handshake_hash(), server.get_handshake_hash());
        Ok((client, server))
    }

    #[test]
    fn transport_authenticates_both_directions_and_rejects_tampering_and_replay() {
        let (client, server) = handshake(None).unwrap();
        let (mut client_send, mut client_receive) = into_session(client).unwrap();
        let (mut server_send, mut server_receive) = into_session(server).unwrap();
        let message = b"synthetic authenticated ACK";
        let cipher = client_send.seal(message).unwrap();

        assert_eq!(cipher.len(), message.len() + TAG_LEN);
        assert!(!cipher.windows(message.len()).any(|part| part == message));
        assert_eq!(server_receive.open(&cipher).unwrap(), message);
        assert!(
            server_receive.open(&cipher).is_err(),
            "replayed ACK was accepted"
        );
        let mut response = server_send.seal(b"synthetic offer").unwrap();
        response[0] ^= 1;

        assert!(
            client_receive.open(&response).is_err(),
            "modified offer was accepted"
        );
    }

    #[test]
    fn wrong_server_pin_cannot_complete_handshake() {
        assert!(handshake(Some([9; 32])).is_err());
        assert!(handshake(None).is_ok(), "a correct server pin must succeed");
    }

    #[test]
    fn records_have_exact_bounds_and_do_not_cross_sessions() {
        use snow::resolvers::CryptoResolver;
        let cipher = snow::resolvers::DefaultResolver
            .resolve_cipher(&snow::params::CipherChoice::ChaChaPoly)
            .unwrap();
        eprintln!(
            "secure state sizes: transport={}, arc_mutex_payload={}, cipher={}, sender={}, receiver={}",
            std::mem::size_of::<TransportState>(),
            std::mem::size_of::<Mutex<TransportState>>() + 2 * std::mem::size_of::<usize>(),
            std::mem::size_of_val(&*cipher),
            std::mem::size_of::<Sender>(),
            std::mem::size_of::<Receiver>()
        );
        let (client, server) = handshake(None).unwrap();
        let (mut send, _) = into_session(client).unwrap();
        let (_, mut receive) = into_session(server).unwrap();
        let message = vec![7; MAX_PLAINTEXT];
        let cipher = send.seal(&message).unwrap();

        assert_eq!(cipher.len(), MAX_CIPHERTEXT);
        assert_eq!(receive.open(&cipher).unwrap(), message);
        assert!(send.seal(&vec![7; MAX_PLAINTEXT + 1]).is_err());
        assert!(receive.open(&vec![0; MAX_CIPHERTEXT + 1]).is_err());
        assert!(receive.open(&[0; TAG_LEN - 1]).is_err());
        let (_, other) = handshake(None).unwrap();
        let (_, mut other_receive) = into_session(other).unwrap();

        assert!(other_receive.open(&cipher).is_err());
    }

    #[test]
    fn public_pins_are_versioned_canonical_and_exactly_32_bytes() {
        let pin = format!("ahrara_server_v1_{}", URL_SAFE_NO_PAD.encode([7; 32]));
        assert_eq!(parse_public_key(&pin).unwrap(), [7; 32]);

        for invalid in [
            URL_SAFE_NO_PAD.encode([7; 32]),
            format!("{pin}="),
            format!(" {pin}"),
            format!("ahrara_server_v1_{}", URL_SAFE_NO_PAD.encode([7; 31])),
            format!("ahrara_server_v2_{}", URL_SAFE_NO_PAD.encode([7; 32])),
        ] {
            assert!(parse_public_key(&invalid).is_err());
        }
    }

    #[test]
    fn generated_private_key_is_not_overwritten_and_has_private_permissions() {
        let directory = std::env::temp_dir().join(format!(
            "ahrara-transport-key-{:032x}",
            rand::random::<u128>()
        ));
        std::fs::create_dir(&directory).unwrap();

        struct Cleanup(std::path::PathBuf);

        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        let _cleanup = Cleanup(directory.clone());
        let path = directory.join("transport.key");
        let pin = keygen(&path).unwrap();
        let public = parse_public_key(&pin).unwrap();

        assert_eq!(public_key(&path).unwrap(), pin);
        let secret = zeroize::Zeroizing::new(std::fs::read(&path).unwrap());

        assert_eq!(secret.len(), 32);
        let secret: [u8; 32] = secret.as_slice().try_into().unwrap();
        let mut client = initiator(&public).unwrap();
        let mut server = responder(&secret).unwrap();
        let mut bytes = [0; MAX_HANDSHAKE];
        let length = client.write_message(&[], &mut bytes).unwrap();

        assert_eq!(
            server
                .read_message(&bytes[..length], &mut [0; MAX_HANDSHAKE])
                .unwrap(),
            0
        );
        assert!(keygen(&path).is_err());
        assert!(
            std::fs::read(&path).unwrap() == secret,
            "private key was overwritten"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

            assert!(
                public_key(&path).is_err(),
                "insecure private key file was accepted"
            );
        }
    }
}
