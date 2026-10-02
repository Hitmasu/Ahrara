use crate::{auth, config::private_dir};
use anyhow::{Result, bail};
use chrono::Utc;
use mailparse::MailHeaderMap;
use rand::{RngCore, rngs::OsRng};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use utoipa::ToSchema;

#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct Address {
    pub id: i64,
    pub identity_fingerprint: String,
    pub route_id: String,
    pub alias_id: String,
    pub address: String,
    pub label: Option<String>,
    pub created_at: String,
    pub enabled: bool,
}

#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct Email {
    pub id: i64,
    pub transfer_id: String,
    pub identity_fingerprint: String,
    pub recipient: String,
    pub envelope_sender: String,
    pub received_at: String,
    #[serde(rename = "from")]
    pub from_header: String,
    #[serde(rename = "to")]
    pub to_header: String,
    pub subject: String,
    pub message_id: Option<String>,
    pub size_bytes: i64,
}

#[derive(Clone, Default, Deserialize, Serialize, schemars::JsonSchema, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Filters {
    pub address: Option<String>,
    pub from_contains: Option<String>,
    pub to_contains: Option<String>,
    pub subject_contains: Option<String>,
    pub after_id: Option<i64>,
    pub limit: Option<u32>,
}
impl Filters {
    pub fn validate(&self) -> Result<()> {
        if self.after_id.is_some_and(|n| n < 0) || self.limit.is_some_and(|n| n == 0 || n > 100) {
            bail!("invalid pagination: limit 1..100, after_id >= 0")
        }
        if let Some(address) = &self.address {
            auth::decode_address(address)?;
        }
        Ok(())
    }
}

pub struct Storage {
    connection: Mutex<Connection>,
}
const EMAIL_COLUMNS: &str = "id,transfer_id,identity_fingerprint,recipient,envelope_sender,received_at,from_header,to_header,subject,message_id,size_bytes";

fn email_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Email> {
    Ok(Email {
        id: r.get(0)?,
        transfer_id: r.get(1)?,
        identity_fingerprint: r.get(2)?,
        recipient: r.get(3)?,
        envelope_sender: r.get(4)?,
        received_at: r.get(5)?,
        from_header: r.get(6)?,
        to_header: r.get(7)?,
        subject: r.get(8)?,
        message_id: r.get(9)?,
        size_bytes: r.get(10)?,
    })
}
fn address_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Address> {
    Ok(Address {
        id: r.get(0)?,
        identity_fingerprint: r.get(1)?,
        route_id: hex::encode(r.get::<_, Vec<u8>>(2)?),
        alias_id: hex::encode(r.get::<_, Vec<u8>>(3)?),
        address: r.get(4)?,
        label: r.get(5)?,
        created_at: r.get(6)?,
        enabled: r.get(7)?,
    })
}

impl Storage {
    pub fn open(path: &Path) -> Result<Arc<Self>> {
        private_dir(
            path.parent()
                .ok_or_else(|| anyhow::anyhow!("database needs parent"))?,
        )?;
        let mut c = Connection::open(path)?;
        // SQLite may return BUSY immediately for concurrent journal-mode
        // transitions, without invoking its busy handler. Retry only this
        // startup transition, with an overall five-second bound.
        c.busy_timeout(Duration::from_millis(50))?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match c.pragma_update(None, "journal_mode", "WAL") {
                Ok(()) => break,
                Err(error)
                    if matches!(
                        error.sqlite_error_code(),
                        Some(
                            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                        )
                    ) && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(25))
                }
                Err(error) => return Err(error.into()),
            }
        }
        c.busy_timeout(Duration::from_secs(5))?;
        c.pragma_update(None, "synchronous", "FULL")?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let version: usize = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let migrations = [
            include_str!("../migrations/001_messages.sql"),
            include_str!("../migrations/002_addresses.sql"),
        ];
        if version > migrations.len() {
            bail!("database schema is newer than this binary")
        }
        for (index, sql) in migrations.iter().enumerate().skip(version) {
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", index + 1)?;
        }
        tx.commit()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(Arc::new(Self {
            connection: Mutex::new(c),
        }))
    }
    pub async fn call<T: Send + 'static>(
        self: &Arc<Self>,
        f: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let storage = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut c = storage
                .connection
                .lock()
                .map_err(|_| anyhow::anyhow!("database lock poisoned"))?;
            f(&mut c)
        })
        .await?
    }
    pub async fn create_address(
        self: &Arc<Self>,
        fingerprint: String,
        route: [u8; 16],
        label: Option<String>,
    ) -> Result<Address> {
        self.create_address_with(fingerprint, route, label, || {
            let mut alias = [0; 8];
            OsRng.fill_bytes(&mut alias);
            alias
        })
        .await
    }
    pub async fn create_address_with(
        self: &Arc<Self>,
        fingerprint: String,
        route: [u8; 16],
        label: Option<String>,
        mut generate: impl FnMut() -> [u8; 8] + Send + 'static,
    ) -> Result<Address> {
        if label.as_ref().is_some_and(|s| s.len() > 256) {
            bail!("label must not exceed 256 UTF-8 bytes")
        }
        self.call(move |c| {
            loop {
                let alias = generate(); let address = auth::address(&route,&alias);
                let result = c.execute("INSERT INTO addresses(identity_fingerprint,route_id,alias_id,address,label,created_at) VALUES (?1,?2,?3,?4,?5,?6)", params![fingerprint,&route[..],&alias[..],address,label,Utc::now().to_rfc3339()]);
                match result {
                    Ok(_) => return Ok(c.query_row("SELECT * FROM addresses WHERE id=?1",[c.last_insert_rowid()],address_row)?),
                    Err(rusqlite::Error::SqliteFailure(e,_)) if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE => continue,
                    Err(e) => return Err(e.into()),
                }
            }
        }).await
    }
    pub async fn addresses(
        self: &Arc<Self>,
        fingerprint: String,
        after: i64,
        limit: u32,
        enabled_only: bool,
    ) -> Result<Vec<Address>> {
        if after < 0 || limit == 0 || limit > 100 {
            bail!("invalid pagination")
        }
        self.call(move |c| {
            let mut q = c.prepare("SELECT * FROM addresses WHERE identity_fingerprint=?1 AND id>?2 AND (?3=0 OR enabled=1) ORDER BY id LIMIT ?4")?;
            Ok(q.query_map(params![fingerprint,after,enabled_only,limit],address_row)?.collect::<rusqlite::Result<_>>()?)
        }).await
    }
    pub async fn address_count(
        self: &Arc<Self>,
        fingerprint: String,
        enabled_only: bool,
    ) -> Result<i64> {
        self.call(move |c| Ok(c.query_row("SELECT count(*) FROM addresses WHERE identity_fingerprint=?1 AND (?2=0 OR enabled=1)",params![fingerprint,enabled_only],|r|r.get(0))?)).await
    }
    pub async fn get_address(self: &Arc<Self>, fingerprint: String, id: String) -> Result<Address> {
        self.call(move |c| Ok(c.query_row("SELECT * FROM addresses WHERE identity_fingerprint=?1 AND (CAST(id AS TEXT)=?2 OR address=?2 COLLATE BINARY)",params![fingerprint,id],address_row)?)).await
    }
    pub async fn set_address(
        self: &Arc<Self>,
        fingerprint: String,
        id: String,
        enabled: Option<bool>,
    ) -> Result<Address> {
        self.call(move |c| {
            let tx = c.transaction()?;
            let mut a = tx.query_row("SELECT * FROM addresses WHERE identity_fingerprint=?1 AND (CAST(id AS TEXT)=?2 OR address=?2 COLLATE BINARY)",params![fingerprint,id],address_row)?;
            if let Some(value)=enabled { tx.execute("UPDATE addresses SET enabled=?1 WHERE id=?2",params![value,a.id])?; a.enabled=value; }
            else { tx.execute("DELETE FROM addresses WHERE id=?1",[a.id])?; }
            tx.commit()?; Ok(a)
        }).await
    }
    pub async fn accepts(self: &Arc<Self>, fingerprint: String, recipient: String) -> Result<bool> {
        self.call(move |c| Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM addresses WHERE identity_fingerprint=?1 AND address=?2 COLLATE BINARY AND enabled=1)",params![fingerprint,recipient],|r|r.get(0))?)).await
    }
    pub async fn insert(
        self: &Arc<Self>,
        transfer: String,
        fingerprint: String,
        recipients: Vec<String>,
        sender: String,
        raw: Vec<u8>,
    ) -> Result<Vec<Email>> {
        if raw.len() > crate::protocol::MAX_EMAIL {
            bail!("message too large")
        }
        self.call(move |c| {
            let headers = mailparse::parse_headers(&raw).map(|x|x.0).unwrap_or_default();
            let header = |name| headers.get_first_value(name).unwrap_or_default().chars().take(4096).collect::<String>();
            let from=header("From"); let to=header("To"); let subject=header("Subject"); let message_id=headers.get_first_value("Message-ID").map(|s|s.chars().take(4096).collect::<String>());
            let received=Utc::now().to_rfc3339();
            let tx=c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let mut saved=Vec::new();
            for recipient in recipients {
                let changed=tx.execute("INSERT INTO messages(transfer_id,identity_fingerprint,recipient,envelope_sender,received_at,from_header,to_header,subject,message_id,size_bytes,raw_eml) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11) ON CONFLICT(transfer_id,recipient) DO NOTHING",params![transfer,fingerprint,recipient,sender,received,from,to,subject,message_id,raw.len() as i64,&raw])?;
                if changed==1 { saved.push(tx.query_row(&format!("SELECT {EMAIL_COLUMNS} FROM messages WHERE id=?1"),[tx.last_insert_rowid()],email_row)?); }
            }
            tx.commit()?; Ok(saved)
        }).await
    }
    pub async fn list(
        self: &Arc<Self>,
        filters: Filters,
        identity: Option<String>,
    ) -> Result<Vec<Email>> {
        filters.validate()?;
        let address = filters
            .address
            .map(|s| auth::decode_address(&s).map(|x| x.2))
            .transpose()?;
        self.call(move |c| {
            let mut q=c.prepare(&format!("SELECT {EMAIL_COLUMNS} FROM messages WHERE id>?1 AND (?2 IS NULL OR recipient=?2 COLLATE BINARY) AND (?3 IS NULL OR instr(lower(from_header || ' ' || envelope_sender),lower(?3))>0) AND (?4 IS NULL OR instr(lower(subject),lower(?4))>0) AND (?5 IS NULL OR instr(lower(to_header || ' ' || recipient),lower(?5))>0) AND (?6 IS NULL OR identity_fingerprint=?6) ORDER BY id LIMIT ?7"))?;
            Ok(q.query_map(params![filters.after_id.unwrap_or(0),address,filters.from_contains,filters.subject_contains,filters.to_contains,identity,filters.limit.unwrap_or(20)],email_row)?.collect::<rusqlite::Result<_>>()?)
        }).await
    }
    pub async fn email(self: &Arc<Self>, id: i64) -> Result<(Email, Vec<u8>)> {
        self.call(move |c| {
            Ok(c.query_row(
                &format!("SELECT {EMAIL_COLUMNS},raw_eml FROM messages WHERE id=?1"),
                [id],
                |r| Ok((email_row(r)?, r.get(11)?)),
            )?)
        })
        .await
    }
    pub async fn raw_slice(
        self: &Arc<Self>,
        id: i64,
        offset: usize,
        max: usize,
    ) -> Result<(Vec<u8>, usize)> {
        self.call(move |c| {
            let total: usize =
                c.query_row("SELECT size_bytes FROM messages WHERE id=?1", [id], |r| {
                    r.get(0)
                })?;
            if offset > total {
                bail!("offset exceeds message size")
            }
            let bytes = c.query_row(
                "SELECT substr(raw_eml,?2,?3) FROM messages WHERE id=?1",
                params![id, offset + 1, max],
                |r| r.get(0),
            )?;
            Ok((bytes, total))
        })
        .await
    }
    pub async fn delete(self: &Arc<Self>, id: Option<i64>) -> Result<usize> {
        self.call(move |c| Ok(c.execute("DELETE FROM messages WHERE ?1 IS NULL OR id=?1", [id])?))
            .await
    }
    pub async fn stats(self: &Arc<Self>) -> Result<(i64, Option<String>, i64)> {
        self.call(|c| {
            Ok(c.query_row(
                "SELECT count(*),max(received_at),coalesce(max(id),0) FROM messages",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .await
    }
    pub async fn schema_version(self: &Arc<Self>) -> Result<i64> {
        self.call(|c| Ok(c.pragma_query_value(None, "user_version", |r| r.get(0))?))
            .await
    }
}
