use crate::config::{Config, private_write};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use hkdf::Hkdf;
use rand::{RngCore, rngs::OsRng};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

pub struct Identity {
    key: SigningKey,
}

#[derive(Clone)]
pub struct DatabaseKey(Zeroizing<[u8; 32]>);

impl DatabaseKey {
    pub(crate) fn sqlcipher_key(&self) -> Zeroizing<String> {
        let hex = Zeroizing::new(hex::encode(self.0.as_slice()));
        Zeroizing::new(format!("x'{}'", hex.as_str()))
    }
}

impl Identity {
    pub fn load_or_create(cfg: &Config) -> Result<(Self, bool)> {
        if cfg.identity_path.exists() {
            return Ok((Self::load(cfg)?, false));
        }
        if cfg.database_path.try_exists()? {
            bail!(
                "existing inbox requires its Recovery Key; register it before opening the profile"
            )
        }

        let key = SigningKey::generate(&mut OsRng);
        match private_write(&cfg.identity_path, &key.to_bytes(), false) {
            Ok(()) => Ok((Self { key }, true)),
            Err(_) if cfg.identity_path.exists() => Ok((Self::load(cfg)?, false)),
            Err(e) => Err(e.context("cannot create identity")),
        }
    }

    pub fn load(cfg: &Config) -> Result<Self> {
        let bytes =
            Zeroizing::new(std::fs::read(&cfg.identity_path).context("cannot read identity")?);
        let seed: &[u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid identity file"))?;

        Ok(Self {
            key: SigningKey::from_bytes(seed),
        })
    }

    pub fn import(recovery: &str, cfg: &Config) -> Result<()> {
        if cfg.database_path.try_exists()? {
            bail!("cannot replace Identity while an inbox exists; use a separate profile")
        }

        let identity = Self::from_recovery(recovery)?;
        let seed = Zeroizing::new(identity.key.to_bytes());
        private_write(&cfg.identity_path, &*seed, true)
    }

    pub fn from_recovery(recovery: &str) -> Result<Self> {
        let encoded = recovery
            .trim()
            .strip_prefix("ahrara_sk_v1_")
            .context("invalid recovery key format")?;
        let bytes = Zeroizing::new(
            URL_SAFE_NO_PAD
                .decode(encoded)
                .map_err(|_| anyhow::anyhow!("invalid recovery key encoding"))?,
        );
        let canonical = Zeroizing::new(URL_SAFE_NO_PAD.encode(&*bytes));
        if bytes.len() != 32 || canonical.as_str() != encoded {
            bail!("invalid recovery key length or encoding")
        }

        Ok(Self {
            key: SigningKey::from_bytes(bytes.as_slice().try_into()?),
        })
    }

    pub fn install_new(recovery: &str, cfg: &Config) -> Result<Self> {
        if cfg.identity_path.try_exists()? {
            bail!("Identity already exists; registration never replaces a key")
        }

        let identity = Self::from_recovery(recovery)?;
        crate::storage::Storage::validate_existing(&cfg.database_path, &identity.database_key())?;
        crate::storage::validate_plaintext_identity(&cfg.database_path, &identity.fingerprint())?;
        let seed = Zeroizing::new(identity.key.to_bytes());
        private_write(&cfg.identity_path, &*seed, false)?;
        Ok(identity)
    }

    pub fn regenerate(cfg: &Config) -> Result<Self> {
        if cfg.database_path.try_exists()? {
            bail!("cannot regenerate Identity while an inbox exists; use a separate profile")
        }

        let key = SigningKey::generate(&mut OsRng);
        private_write(&cfg.identity_path, &key.to_bytes(), true)?;
        Ok(Self { key })
    }

    pub fn export(&self) -> Zeroizing<String> {
        Zeroizing::new(format!(
            "ahrara_sk_v1_{}",
            URL_SAFE_NO_PAD.encode(self.key.to_bytes())
        ))
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    pub fn fingerprint(&self) -> String {
        hex::encode(Sha256::digest(self.public_key()))
    }

    pub fn route(&self) -> [u8; 16] {
        derive_route(&self.public_key())
    }

    pub fn database_key(&self) -> DatabaseKey {
        let seed = Zeroizing::new(self.key.to_bytes());
        let mut key = Zeroizing::new([0; 32]);
        Hkdf::<Sha256>::new(Some(b"ahrara-identity-kdf-v1\0"), &*seed)
            .expand(b"ahrara-sqlcipher-v1\0", &mut *key)
            .expect("32 bytes is a valid HKDF-SHA256 output length");

        DatabaseKey(key)
    }

    pub fn sign_bound(&self, challenge: &[u8], binding: &[u8; 32]) -> Result<[u8; 64]> {
        let challenge: &[u8; 32] = challenge
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid authentication challenge"))?;

        Ok(self
            .key
            .sign(&crate::secure::auth_preimage(
                &self.public_key(),
                challenge,
                binding,
            ))
            .to_bytes())
    }

    pub fn sign(&self, challenge: &[u8]) -> Result<[u8; 64]> {
        if challenge.len() != 32 {
            bail!("invalid authentication challenge")
        }

        let mut bytes = b"ahrara-auth-v1\0".to_vec();
        bytes.extend(self.public_key());
        bytes.extend(challenge);
        Ok(self.key.sign(&bytes).to_bytes())
    }
}

pub fn derive_route(public: &[u8; 32]) -> [u8; 16] {
    let mut h = Sha256::new();
    h.update(b"ahrara-route-v1\0");
    h.update(public);
    h.finalize()[..16].try_into().expect("16 bytes")
}

pub fn address(route: &[u8; 16], alias: &[u8; 8]) -> String {
    let mut bytes = [0; 24];
    bytes[..16].copy_from_slice(route);
    bytes[16..].copy_from_slice(alias);
    format!("{}@ahrara.dev", URL_SAFE_NO_PAD.encode(bytes))
}

pub fn decode_address(value: &str) -> Result<([u8; 16], [u8; 8], String)> {
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

pub fn generate_token(cfg: &Config, overwrite: bool) -> Result<()> {
    let mut bytes = Zeroizing::new([0; 32]);
    OsRng.fill_bytes(&mut *bytes);
    let token = Zeroizing::new(URL_SAFE_NO_PAD.encode(*bytes));
    private_write(&cfg.api_token_path, token.as_bytes(), overwrite)
}

pub fn validate_api_token(value: &str) -> Result<()> {
    let decoded = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|_| anyhow::anyhow!("invalid local API token file"))?,
    );
    if decoded.len() != 32 || value.len() != 43 {
        bail!("invalid local API token file")
    }

    Ok(())
}
