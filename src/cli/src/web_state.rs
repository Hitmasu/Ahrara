use crate::{
    config::{Config, private_write},
    core::Core,
};
use anyhow::{Context, Result, bail};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    sync::{Mutex, atomic::AtomicBool},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use zeroize::Zeroizing;

pub const LAN_WARNING: &str = "LAN access will expose the Ahrara Web Page to other devices\non your network.\n\nNo password is configured.\n\nAnyone who can reach this device on the local network\nwill be able to access your locally stored emails.\n\nContinue without authentication?";

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub web_enabled: bool,
    pub api_enabled: bool,
    pub lan_enabled: bool,
    pub lan_no_auth_acknowledged: bool,
    pub mcp_enabled: bool,
    pub password_hash: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            web_enabled: true,
            api_enabled: false,
            lan_enabled: false,
            lan_no_auth_acknowledged: false,
            mcp_enabled: true,
            password_hash: None,
        }
    }
}

pub struct WebState {
    settings: Mutex<Settings>,
    pub csrf: String,
    sessions: Mutex<HashMap<String, Instant>>,
    attempts: Mutex<HashMap<IpAddr, (u32, Instant)>>,
    pub password_work: Semaphore,
    pub api_forced: AtomicBool,
    pub status: Mutex<String>,
}

impl WebState {
    pub fn load(cfg: &Config) -> Result<Self> {
        let path = cfg.profile_dir().join("interfaces.toml");
        let settings: Settings = if path.exists() {
            toml::from_str(&std::fs::read_to_string(path)?)?
        } else {
            Settings::default()
        };
        if let Some(hash) = &settings.password_hash {
            let parsed = PasswordHash::new(hash)
                .map_err(|_| anyhow::anyhow!("invalid Web password hash"))?;
            if parsed.algorithm.as_str() != "argon2id" {
                bail!("Web password must use Argon2id")
            }
        }
        if settings.lan_enabled
            && settings.password_hash.is_none()
            && !settings.lan_no_auth_acknowledged
        {
            bail!("LAN without a password needs explicit acknowledgement; use --lan interactively")
        }

        Ok(Self {
            settings: Mutex::new(settings),
            csrf: random_secret(),
            sessions: Mutex::new(HashMap::new()),
            attempts: Mutex::new(HashMap::new()),
            password_work: Semaphore::new(2),
            api_forced: AtomicBool::new(false),
            status: Mutex::new("Disabled".into()),
        })
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    pub fn authenticated(&self, session: Option<&str>) -> bool {
        if self.settings().password_hash.is_none() {
            return true;
        }

        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|_, until| *until > Instant::now());
        session.is_some_and(|s| sessions.contains_key(s))
    }

    pub fn session(&self, expected_hash: &str) -> Option<String> {
        let settings = self.settings.lock().unwrap();
        if settings.password_hash.as_deref() != Some(expected_hash) {
            return None;
        }

        let token = random_secret();
        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|_, until| *until > Instant::now());
        // Bounded local sessions; reaching capacity signs out the oldest session.
        if sessions.len() >= 64
            && let Some(oldest) = sessions
                .iter()
                .min_by_key(|(_, until)| **until)
                .map(|(id, _)| id.clone())
        {
            sessions.remove(&oldest);
        }

        sessions.insert(
            token.clone(),
            Instant::now() + Duration::from_secs(12 * 3600),
        );

        Some(token)
    }

    pub fn logout(&self, session: Option<&str>) {
        if let Some(s) = session {
            self.sessions.lock().unwrap().remove(s);
        }
    }

    pub fn allow_login(&self, ip: IpAddr) -> bool {
        let ip = login_client(ip);
        let mut attempts = self.attempts.lock().unwrap();
        let now = Instant::now();
        attempts.retain(|_, (_, until)| *until + Duration::from_secs(300) > now);
        if attempts.get(&ip).is_some_and(|(_, until)| *until > now) {
            return false;
        }
        // Bounded state; evicting the earliest-expiring entry keeps the most
        // throttled clients without locking out every new address.
        if attempts.len() >= 256
            && !attempts.contains_key(&ip)
            && let Some(oldest) = attempts
                .iter()
                .min_by_key(|(_, (_, until))| *until)
                .map(|(ip, _)| *ip)
        {
            attempts.remove(&oldest);
        }

        let (count, until) = attempts.entry(ip).or_insert((0, now));
        *count = count.saturating_add(1);
        *until = now
            + Duration::from_secs(if *count <= 3 {
                0
            } else {
                1u64 << (*count - 4).min(5)
            });

        true
    }

    pub fn login_succeeded(&self, ip: IpAddr) {
        self.attempts.lock().unwrap().remove(&login_client(ip));
    }
}

// One IPv6 client usually controls a whole /64 and any local process every
// loopback address; IPv4-mapped peers stay IPv4.
fn login_client(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        ip if ip.is_loopback() => IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) => IpAddr::V6((u128::from(ip) & (u128::MAX << 64)).into()),
        ip => ip,
    }
}

pub fn random_secret() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

pub fn verify_password(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash).ok().is_some_and(|hash| {
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    })
}

impl Core {
    // All interfaces use this single path for durable preferences and event publication.
    pub async fn update_interfaces(
        self: &std::sync::Arc<Self>,
        action: &str,
        enabled: bool,
        acknowledge: bool,
        password: Option<Zeroizing<String>>,
    ) -> Result<()> {
        if enabled
            && (action == "lan" || self.https_enabled() && matches!(action, "web" | "api" | "mcp"))
        {
            self.validate_http_tls().await?;
        }

        let new_hash = if let Some(password) = password {
            if !(8..=1024).contains(&password.len()) {
                bail!("Use a password of 8–1024 bytes")
            }

            let permit = self.web.password_work.acquire().await?;
            let hash = tokio::task::spawn_blocking(move || {
                Argon2::default()
                    .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
                    .map(|hash| hash.to_string())
                    .map_err(|_| anyhow::anyhow!("password hashing failed"))
            })
            .await??;
            drop(permit);
            Some(hash)
        } else {
            None
        };
        let core = self.clone();
        let action = action.to_owned();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut current = core.web.settings.lock().unwrap();
            let mut next = current.clone();
            match action.as_str() {
                "web" => next.web_enabled = enabled,
                "api" => next.api_enabled = enabled,
                "mcp" => next.mcp_enabled = enabled,
                "lan" => {
                    next.lan_enabled = enabled;
                    if enabled {
                        next.web_enabled = true;
                    }
                }
                "password" => {
                    next.password_hash = Some(new_hash.context("new password required")?);
                    next.lan_no_auth_acknowledged = false;
                }
                "remove_password" => {
                    next.password_hash = None;
                    next.lan_no_auth_acknowledged = false;
                }
                _ => bail!("unknown interface setting"),
            }
            if next.lan_enabled && next.password_hash.is_none() && !next.lan_no_auth_acknowledged {
                if !acknowledge {
                    bail!("LAN acknowledgement required")
                }

                next.lan_no_auth_acknowledged = true;
            }

            private_write(
                &core.config.profile_dir().join("interfaces.toml"),
                toml::to_string(&next)?.as_bytes(),
                true,
            )?;
            if current.password_hash != next.password_hash {
                core.web.sessions.lock().unwrap().clear();
            }

            *current = next;
            drop(current);
            core.emit(
                if action == "mcp" {
                    "mcp_state_changed"
                } else {
                    "web_settings_changed"
                },
                json!({"changed": action}),
            );

            Ok(())
        })
        .await?
    }

    pub async fn validate_http_tls(&self) -> Result<axum_server::tls_rustls::RustlsConfig> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        axum_server::tls_rustls::RustlsConfig::from_pem_file(&self.config.api_tls_cert_path, &self.config.api_tls_key_path).await.context("Web/API/MCP requires valid TLS certificate and key; configure api_tls_cert_path and api_tls_key_path")
    }

    pub fn web_url(&self) -> String {
        let Ok(mut addr) = self.config.api_bind.parse::<SocketAddr>() else {
            return "Invalid bind address".into();
        };
        if self.web.settings().lan_enabled {
            // UDP connect chooses the default route without transmitting packets.
            let (bind, probe, fallback) = if addr.is_ipv4() {
                ("0.0.0.0:0", "192.0.2.1:9", IpAddr::V4(Ipv4Addr::LOCALHOST))
            } else {
                (
                    "[::]:0",
                    "[2001:db8::1]:9",
                    IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
                )
            };
            let ip = UdpSocket::bind(bind)
                .ok()
                .and_then(|socket| {
                    socket.connect(probe).ok()?;
                    socket.local_addr().ok().map(|a| a.ip())
                })
                .filter(|ip| match ip {
                    IpAddr::V4(v4) => v4.is_private(),
                    IpAddr::V6(v6) => v6.is_unique_local(),
                });

            addr.set_ip(ip.unwrap_or(fallback));
        }

        format!("{}://{addr}", self.http_scheme())
    }

    pub fn https_enabled(&self) -> bool {
        self.config.api_https || self.web.settings().lan_enabled
    }

    pub fn http_scheme(&self) -> &'static str {
        if self.https_enabled() {
            "https"
        } else {
            "http"
        }
    }

    pub fn mcp_auth_required(&self) -> bool {
        self.config.mcp_auth || self.web.settings().lan_enabled
    }
}
