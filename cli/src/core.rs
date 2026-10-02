use crate::{
    auth::Identity,
    config::{Config, private_dir, private_write},
    mail,
    storage::{Address, Email, Filters, Storage},
};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, Write},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, broadcast, watch};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Serialize)]
pub struct Event {
    pub kind: String,
    pub data: Value,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ConnectionState {
    pub connected: bool,
    pub state: String,
    pub last_error: Option<String>,
}
#[derive(Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Owner {
    pub mode: String,
    pub pid: u32,
    pub fingerprint: String,
}
#[derive(Serialize, utoipa::ToSchema)]
pub struct Status {
    pub connected: bool,
    pub status: String,
    pub last_error: Option<String>,
    pub identity_fingerprint: String,
    pub route_id: String,
    pub addresses: Vec<Address>,
    pub enabled_addresses_count: i64,
    pub next_cursor: Option<i64>,
    pub emails_count: i64,
    pub last_email_at: Option<String>,
    pub server: String,
    pub owner: Option<Owner>,
    pub cursor: i64,
}
#[derive(Serialize, utoipa::ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WaitResult {
    Matched { email: Box<mail::ParsedEmail> },
    Timeout,
}
pub struct Ownership {
    file: File,
}
impl Drop for Ownership {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

impl Ownership {
    pub fn acquire(cfg: &Config, mode: &str, fingerprint: &str) -> Result<Self> {
        Self::acquire_file(cfg, "listener.lock", mode, fingerprint)
    }
    pub fn acquire_http(cfg: &Config, fingerprint: &str) -> Result<Self> {
        Self::acquire_file(cfg, "http.lock", "local server", fingerprint)
    }
    fn acquire_file(cfg: &Config, name: &str, mode: &str, fingerprint: &str) -> Result<Self> {
        private_dir(cfg.profile_dir())?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(cfg.profile_dir().join(name))?;
        if FileExt::try_lock_exclusive(&file).is_err() {
            let mut text = String::new();
            file.read_to_string(&mut text)?;
            let owner = serde_json::from_str::<Owner>(&text).ok();
            bail!(
                "listener_owned: {}",
                owner
                    .map(|o| format!("ahrara {} (PID {})", o.mode, o.pid))
                    .unwrap_or_else(|| "another local process".into())
            )
        }
        let owner = Owner {
            mode: mode.into(),
            pid: std::process::id(),
            fingerprint: fingerprint.into(),
        };
        file.set_len(0)?;
        file.rewind()?;
        serde_json::to_writer(&mut file, &owner)?;
        file.flush()?;
        Ok(Self { file })
    }
    pub fn current(cfg: &Config) -> Result<Option<Owner>> {
        Self::current_file(cfg, "listener.lock")
    }
    pub fn current_http(cfg: &Config) -> Result<Option<Owner>> {
        Self::current_file(cfg, "http.lock")
    }
    fn current_file(cfg: &Config, name: &str) -> Result<Option<Owner>> {
        let path = cfg.profile_dir().join(name);
        if !path.exists() {
            return Ok(None);
        }
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        if FileExt::try_lock_exclusive(&file).is_ok() {
            FileExt::unlock(&file)?;
            return Ok(None);
        }
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        Ok(Some(serde_json::from_str(&text).unwrap_or(Owner {
            mode: "starting".into(),
            pid: 0,
            fingerprint: String::new(),
        })))
    }
}

struct Running {
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}
pub struct Core {
    pub config: Config,
    pub identity: Identity,
    pub storage: Arc<Storage>,
    pub events: broadcast::Sender<Event>,
    pub state: watch::Sender<ConnectionState>,
    pub start_cursor: AtomicI64,
    pub web: crate::web_state::WebState,
    running: Mutex<Option<Running>>,
}

impl Core {
    pub fn open(config: Config) -> Result<(Arc<Self>, bool)> {
        let (identity, created) = Identity::load_or_create(&config)?;
        let _migration = if Storage::is_plaintext(&config.database_path)? {
            Some(Ownership::acquire(
                &config,
                "database encryption",
                &identity.fingerprint(),
            )?)
        } else {
            None
        };
        let storage = Storage::open(&config.database_path, &identity.database_key())?;
        let (events, _) = broadcast::channel(256);
        let (state, _) = watch::channel(ConnectionState {
            state: "stopped".into(),
            ..Default::default()
        });
        Ok((
            Arc::new(Self {
                web: crate::web_state::WebState::load(&config)?,
                config,
                identity,
                storage,
                events,
                state,
                start_cursor: AtomicI64::new(0),
                running: Mutex::new(None),
            }),
            created,
        ))
    }
    pub fn emit(&self, kind: &str, data: Value) {
        let _ = self.events.send(Event {
            kind: kind.into(),
            data,
        });
    }
    pub async fn set_state(&self, state: &str, error: Option<String>) {
        let value = ConnectionState {
            connected: state == "connected",
            state: state.into(),
            last_error: error,
        };
        self.state.send_replace(value.clone());
        let path = self.config.profile_dir().join("listener.status.json");
        if let Ok(bytes) = serde_json::to_vec(&value) {
            let _ = tokio::task::spawn_blocking(move || private_write(&path, &bytes, true)).await;
        }
        self.emit("connection_state_changed", json!(value));
    }
    pub async fn create_address(&self, label: Option<String>) -> Result<Address> {
        let address = self
            .storage
            .create_address(self.identity.fingerprint(), self.identity.route(), label)
            .await?;
        self.emit("address_created", json!(address));
        Ok(address)
    }
    pub async fn change_address(&self, id: String, enabled: Option<bool>) -> Result<Address> {
        let address = self
            .storage
            .set_address(self.identity.fingerprint(), id, enabled)
            .await?;
        self.emit(
            if enabled.is_some() {
                "address_updated"
            } else {
                "address_deleted"
            },
            json!(address),
        );
        Ok(address)
    }
    pub async fn start(self: &Arc<Self>, mode: &str, wait: bool) -> Result<Status> {
        {
            let mut running = self.running.lock().await;
            if running.as_ref().is_some_and(|r| r.task.is_finished()) {
                running.take();
            }
            if running.is_none() {
                let ownership =
                    Ownership::acquire(&self.config, mode, &self.identity.fingerprint())?;
                if self
                    .storage
                    .address_count(self.identity.fingerprint(), false)
                    .await?
                    == 0
                {
                    self.create_address(None).await?;
                }
                self.start_cursor
                    .store(self.storage.stats().await?.2, Ordering::Relaxed);
                let cancel = CancellationToken::new();
                let worker_cancel = cancel.clone();
                let core = self.clone();
                self.set_state("connecting", None).await;
                let task = tokio::spawn(async move {
                    let _ownership = ownership;
                    crate::transport::run(core, worker_cancel).await;
                });
                *running = Some(Running { cancel, task });
            }
        }
        if wait {
            let mut state = self.state.subscribe();
            let result = tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    let current = state.borrow().clone();
                    if current.connected {
                        return Ok(());
                    }
                    if current.state == "failed" {
                        bail!(
                            "{}",
                            current
                                .last_error
                                .unwrap_or_else(|| "connection failed".into())
                        )
                    }
                    state.changed().await.context("listener stopped")?;
                }
            })
            .await;
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    self.stop().await;
                    return Err(e);
                }
                Err(_) => {
                    self.stop().await;
                    bail!("connection timed out")
                }
            }
        }
        self.status().await
    }
    pub async fn stop(&self) {
        let mut running = self.running.lock().await;
        if let Some(r) = running.take() {
            r.cancel.cancel();
            let _ = r.task.await;
        }
    }
    pub async fn status(&self) -> Result<Status> {
        let owner = Ownership::current(&self.config)?;
        let mut state = self.state.borrow().clone();
        if owner.is_some() && self.running.lock().await.is_none() {
            state = std::fs::read(self.config.profile_dir().join("listener.status.json"))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or(ConnectionState {
                    state: "starting".into(),
                    ..Default::default()
                });
        }
        if owner.is_none() {
            state = ConnectionState {
                state: "stopped".into(),
                ..Default::default()
            };
        }
        let (count, last, _) = self.storage.stats().await?;
        let addresses = self
            .storage
            .addresses(self.identity.fingerprint(), 0, 20, true)
            .await?;
        let addresses_count = self
            .storage
            .address_count(self.identity.fingerprint(), true)
            .await?;
        let next_cursor = if addresses_count > addresses.len() as i64 {
            addresses.last().map(|a| a.id)
        } else {
            None
        };
        Ok(Status {
            connected: state.connected,
            status: state.state,
            last_error: state.last_error,
            identity_fingerprint: self.identity.fingerprint(),
            route_id: hex::encode(self.identity.route()),
            addresses,
            enabled_addresses_count: addresses_count,
            next_cursor,
            emails_count: count,
            last_email_at: last,
            server: self.config.server_url.clone(),
            owner,
            cursor: self.start_cursor.load(Ordering::Relaxed),
        })
    }
    pub async fn wait_email(&self, mut filters: Filters, timeout: u64) -> Result<WaitResult> {
        if !(1..=300).contains(&timeout) {
            bail!("timeout must be 1..300 seconds")
        }
        filters.validate()?;
        if let Some(address) = &filters.address
            && crate::auth::decode_address(&self.storage.routing_filter(address.clone()).await?)?.0
                != self.identity.route()
        {
            bail!("address belongs to another Identity")
        }
        if self.running.lock().await.is_none() {
            bail!("listener_not_started")
        }
        let mut events = self.events.subscribe();
        if filters.after_id.is_none() {
            filters.after_id = Some(self.start_cursor.load(Ordering::Relaxed));
        }
        filters.limit = Some(1);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout);
        loop {
            if let Some(email) = self
                .storage
                .list(filters.clone(), Some(self.identity.fingerprint()))
                .await?
                .into_iter()
                .next()
            {
                let parsed = self.get_email(email.id, 2048).await?;
                return Ok(WaitResult::Matched {
                    email: Box::new(parsed),
                });
            }
            if self.state.borrow().state == "stopped" {
                bail!("listener_stopped")
            }
            tokio::select! {
                _=tokio::time::sleep_until(deadline)=>return Ok(WaitResult::Timeout),
                event=events.recv()=> { match event { Ok(_)|Err(broadcast::error::RecvError::Lagged(_))=>{},Err(_)=>bail!("listener_stopped") } }
            }
        }
    }
    pub async fn get_email(&self, id: i64, max: usize) -> Result<mail::ParsedEmail> {
        let (meta, raw) = self.storage.email(id).await?;
        tokio::task::spawn_blocking(move || mail::parse(meta, &raw, max)).await?
    }
    pub async fn read_eml(&self, id: i64, offset: usize, max: usize) -> Result<Value> {
        if max == 0 || max > 256 * 1024 {
            bail!("max_bytes must be 1..262144")
        }
        let (bytes, total) = self.storage.raw_slice(id, offset, max).await?;
        let (encoding, content) = match std::str::from_utf8(&bytes) {
            Ok(s) => ("utf8", s.to_owned()),
            Err(_) => ("base64", STANDARD.encode(&bytes)),
        };
        Ok(
            json!({"encoding":encoding,"content":content,"offset":offset,"returned_bytes":bytes.len(),"total_bytes":total,"has_more":offset+bytes.len()<total,"next_offset":offset+bytes.len()}),
        )
    }
    pub async fn attachment(
        &self,
        id: i64,
        attachment_id: String,
    ) -> Result<(mail::Attachment, Vec<u8>)> {
        let (_, raw) = self.storage.email(id).await?;
        tokio::task::spawn_blocking(move || mail::attachment(&raw, &attachment_id)).await?
    }
    pub async fn save_attachment(
        &self,
        id: i64,
        attachment_id: String,
        destination: &Path,
    ) -> Result<Value> {
        let (meta, bytes) = self.attachment(id, attachment_id).await?;
        let destination = destination.to_owned();
        let path =
            tokio::task::spawn_blocking(move || mail::save_attachment(&meta, &bytes, &destination))
                .await??;
        Ok(json!({"path":path,"saved":true}))
    }
    pub fn received(&self, emails: Vec<Email>) {
        for email in emails {
            self.emit("email_received", json!(email));
        }
    }

    pub async fn delete_emails(&self, id: Option<i64>) -> Result<usize> {
        let deleted = self.storage.delete(id).await?;
        self.emit(
            if id.is_some() {
                "email_deleted"
            } else {
                "emails_cleared"
            },
            json!({"id":id,"deleted":deleted}),
        );
        Ok(deleted)
    }

    pub async fn html_email(&self, id: i64, allow_remote: bool) -> Result<mail::HtmlEmail> {
        let (_, raw) = self.storage.email(id).await?;
        tokio::task::spawn_blocking(move || mail::html_body(&raw, allow_remote)).await?
    }
}
