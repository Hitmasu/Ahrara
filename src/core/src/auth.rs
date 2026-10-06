#[cfg(not(target_arch = "wasm32"))]
use crate::config::{Config, private_write};
pub use crate::protocol::{decode_address, derive_route};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use hkdf::Hkdf;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

#[derive(Clone)]
pub struct Identity {
    key: SigningKey,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
pub struct DatabaseKey(Zeroizing<[u8; 32]>);

#[cfg(not(target_arch = "wasm32"))]
impl DatabaseKey {
    pub(crate) fn sqlcipher_key(&self) -> Zeroizing<String> {
        let hex = Zeroizing::new(hex::encode(self.0.as_slice()));
        Zeroizing::new(format!("x'{}'", hex.as_str()))
    }
}

/// Identities stored as files; the browser keeps its seed in its own storage.
#[cfg(not(target_arch = "wasm32"))]
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

    pub fn install_new(recovery: &str, cfg: &Config) -> Result<Self> {
        if cfg.identity_path.try_exists()? {
            bail!("Identity already exists; registration never replaces a key")
        }

        let identity = Self::from_recovery(recovery)?;
        crate::storage::Storage::validate_existing(&cfg.database_path, &identity.database_key())?;
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
}

impl Identity {
    pub fn generate() -> Self {
        Self {
            key: SigningKey::generate(&mut OsRng),
        }
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

    #[cfg(not(target_arch = "wasm32"))]
    pub fn database_key(&self) -> DatabaseKey {
        DatabaseKey(self.derive_key(b"ahrara-sqlcipher-v1\0"))
    }

    /// Encrypts the browser's stored mail, as `database_key` does for SQLCipher.
    pub fn browser_store_key(&self) -> Zeroizing<[u8; 32]> {
        self.derive_key(b"ahrara-web-store-v1\0")
    }

    fn derive_key(&self, info: &[u8]) -> Zeroizing<[u8; 32]> {
        let seed = Zeroizing::new(self.key.to_bytes());
        let mut key = Zeroizing::new([0; 32]);
        Hkdf::<Sha256>::new(Some(b"ahrara-identity-kdf-v1\0"), &*seed)
            .expand(info, &mut *key)
            .expect("32 bytes is a valid HKDF-SHA256 output length");

        key
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
}

pub fn address(route: &[u8; 16], alias: &[u8; 8]) -> String {
    let mut bytes = [0; 24];
    bytes[..16].copy_from_slice(route);
    bytes[16..].copy_from_slice(alias);
    format!("{}@ahrara.dev", URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn generate_token(cfg: &Config, overwrite: bool) -> Result<()> {
    use rand::RngCore;
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
