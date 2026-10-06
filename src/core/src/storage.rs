pub use crate::model::{Address, CustomDomain, Email, Filters};
use crate::{
    auth,
    config::private_dir,
    model::{address_token, normalize_domain},
};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use mailparse::MailHeaderMap;
use rand::{RngCore, rngs::OsRng};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params, params_from_iter, types::Value};
use std::{
    fs::{File, OpenOptions, TryLockError},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub struct Storage {
    // ponytail: one connection serializes local queries. Consider a read pool
    // only if measurements still show reads delaying delivery after indexing.
    connection: Mutex<Connection>,
    // Keep the shared lock until after the SQLite connection has closed.
    _database_lock: File,
    key: auth::DatabaseKey,
    path: PathBuf,
    // Declared last so database and lock handles close before directory cleanup.
    _temporary_profile: Option<tempfile::TempDir>,
}

const MIGRATIONS: [&str; 5] = [
    include_str!("../migrations/001_messages.sql"),
    include_str!("../migrations/002_addresses.sql"),
    include_str!("../migrations/003_address_queries.sql"),
    include_str!("../migrations/004_custom_domain.sql"),
    include_str!("../migrations/005_sdk_receipts.sql"),
];
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
    let routing_address: String = r.get(4)?;
    Ok(Address {
        id: r.get(0)?,
        identity_fingerprint: r.get(1)?,
        route_id: hex::encode(r.get::<_, Vec<u8>>(2)?),
        alias_id: hex::encode(r.get::<_, Vec<u8>>(3)?),
        address: r
            .get::<_, Option<String>>(8)?
            .unwrap_or_else(|| routing_address.clone()),
        forwarding_address: r
            .get::<_, Option<String>>(9)?
            .unwrap_or_else(|| routing_address.clone()),
        routing_address,
        label: r.get(5)?,
        created_at: r.get(6)?,
        enabled: r.get(7)?,
    })
}

fn find_address(c: &Connection, fingerprint: &str, selector: &str) -> rusqlite::Result<Address> {
    // Avoid CAST(id AS TEXT): it disables the primary-key lookup. Only accept
    // canonical numeric IDs, retaining the previous selector semantics.
    if let Ok(id) = selector.parse::<i64>()
        && id.to_string() == selector
    {
        c.prepare_cached("SELECT * FROM addresses WHERE identity_fingerprint=?1 AND id=?2")?
            .query_row(params![fingerprint, id], address_row)
    } else {
        let canonical = c.prepare_cached("SELECT * FROM addresses WHERE identity_fingerprint=?1 AND address=?2 COLLATE BINARY")?
            .query_row(params![fingerprint, selector], address_row).optional()?;
        match canonical {
            Some(address) => Ok(address),
            None => c.prepare_cached("SELECT * FROM addresses WHERE identity_fingerprint=?1 AND public_address=?2 COLLATE BINARY")?
                .query_row(params![fingerprint, selector], address_row),
        }
    }
}

fn read_custom_domain(c: &Connection, fingerprint: &str) -> Result<Option<CustomDomain>> {
    Ok(c.query_row(
        "SELECT domain,forwarding_address FROM custom_domains WHERE identity_fingerprint=?1",
        [fingerprint],
        |r| {
            Ok(CustomDomain {
                domain: r.get(0)?,
                forwarding_address: r.get(1)?,
            })
        },
    )
    .optional()?)
}

fn database_lock(path: &Path, suffix: &str, exclusive: bool, wait: bool) -> Result<File> {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(Path::new(&name))?;
    let deadline = Instant::now() + Duration::from_secs(5);

    loop {
        let result = if exclusive {
            file.try_lock()
        } else {
            file.try_lock_shared()
        };

        match result {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) => {
                if !wait || Instant::now() >= deadline {
                    bail!("inbox is in use; close other clients before replacing or encrypting it")
                }

                std::thread::sleep(Duration::from_millis(25));
            }
            Err(TryLockError::Error(error)) => return Err(error.into()),
        }
    }
}

fn set_key(c: &Connection, key: &auth::DatabaseKey) -> Result<()> {
    let version: Option<String> = c
        .query_row("PRAGMA cipher_version", [], |r| r.get(0))
        .optional()?;
    if version.as_deref().is_none_or(str::is_empty) {
        bail!("this client was built without SQLCipher encryption")
    }

    let key = key.sqlcipher_key();
    c.pragma_update(None, "key", key.as_str())
        .map_err(|_| anyhow::anyhow!("cannot configure inbox encryption"))?;
    Ok(())
}

fn encrypted_connection(
    path: &Path,
    key: &auth::DatabaseKey,
    flags: OpenFlags,
) -> Result<Connection> {
    let c = Connection::open_with_flags(path, flags)?;
    set_key(&c, key)?;
    c.query_row("SELECT count(*) FROM sqlite_schema", [], |r| {
        r.get::<_, i64>(0)
    })
    .map_err(|_| anyhow::anyhow!("incorrect inbox key or damaged encrypted database"))?;
    c.busy_timeout(Duration::from_secs(5))?;
    Ok(c)
}

fn apply_migrations(c: &mut Connection) -> Result<()> {
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let version: usize = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version > MIGRATIONS.len() {
        bail!("database schema is newer than this binary")
    }

    for (index, sql) in MIGRATIONS.iter().enumerate().skip(version) {
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", index + 1)?;
    }

    tx.commit()?;
    Ok(())
}

fn validate_database(c: &Connection, current_schema: bool) -> Result<()> {
    let integrity: String = c.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if integrity != "ok" {
        bail!("inbox integrity check failed")
    }
    if c.prepare("PRAGMA cipher_integrity_check")?
        .query([])?
        .next()?
        .is_some()
    {
        bail!("encrypted inbox integrity check failed")
    }

    let version: usize = c.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version > MIGRATIONS.len() || (current_schema && version != MIGRATIONS.len()) {
        bail!("unsupported inbox schema version")
    }
    if current_schema {
        c.prepare(&format!(
            "SELECT {EMAIL_COLUMNS},raw_eml FROM messages LIMIT 0"
        ))?;
        c.prepare("SELECT id,identity_fingerprint,route_id,alias_id,address,label,created_at,enabled,public_address,forwarding_address FROM addresses LIMIT 0")?;
        c.prepare(
            "SELECT identity_fingerprint,domain,forwarding_address FROM custom_domains LIMIT 0",
        )?;
        let schema = |db: &Connection| -> Result<Vec<(String, String, String)>> {
            Ok(db
                .prepare("SELECT type,name,sql FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*' ORDER BY type,name")?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?)
        };
        let mut reference = Connection::open_in_memory()?;
        apply_migrations(&mut reference)?;
        if schema(c)? != schema(&reference)? {
            bail!("inbox schema does not match its migrations")
        }
    }

    Ok(())
}

fn database_uri(path: &Path, mode: &str) -> Result<String> {
    let mut uri = url::Url::from_file_path(std::path::absolute(path)?)
        .map_err(|_| anyhow::anyhow!("invalid inbox file path"))?;
    uri.query_pairs_mut().append_pair("mode", mode);
    Ok(uri.into())
}

fn export_attached(c: &mut Connection, path: &Path, key: &auth::DatabaseKey) -> Result<()> {
    let key = key.sqlcipher_key();
    c.execute(
        "ATTACH DATABASE ?1 AS ahrara_export KEY ?2",
        params![database_uri(path, "rwc")?, key.as_str()],
    )
    .map_err(|_| anyhow::anyhow!("cannot create encrypted inbox snapshot"))?;
    let result: Result<()> = (|| {
        c.pragma_update(Some("ahrara_export"), "synchronous", "FULL")?;
        let tx = c.transaction()?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        tx.execute_batch("SELECT sqlcipher_export('ahrara_export');")?;
        tx.pragma_update(Some("ahrara_export"), "user_version", version)?;
        tx.commit()?;
        Ok(())
    })();
    let detached = c.execute_batch("DETACH DATABASE ahrara_export;");
    result?;
    detached?;
    Ok(())
}

fn sync_parent(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path.parent().context("inbox needs parent directory")?)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn routing_filter(c: &Connection, address: &str) -> Result<String> {
    if let Ok((_, _, canonical)) = auth::decode_address(address) {
        return Ok(canonical);
    }

    address_token(address)?;
    let (token, domain) = address.split_once('@').unwrap();
    let public = format!("{token}@{}", normalize_domain(domain)?);
    c.query_row(
        "SELECT forwarding_address FROM addresses WHERE public_address=?1 COLLATE BINARY",
        [public],
        |r| r.get(0),
    )
    .map_err(Into::into)
}

impl Storage {
    pub(crate) fn retain_profile(&mut self, directory: tempfile::TempDir) {
        self._temporary_profile = Some(directory);
    }

    pub fn validate_existing(path: &Path, key: &auth::DatabaseKey) -> Result<()> {
        if !path.try_exists()? || path.metadata()?.len() == 0 {
            return Ok(());
        }

        let _opening = database_lock(path, ".open.lock", true, true)?;
        let _reading = database_lock(path, ".lock", false, true)?;
        let c = encrypted_connection(path, key, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        validate_database(&c, false)
    }

    pub fn open(path: &Path, key: &auth::DatabaseKey) -> Result<Arc<Self>> {
        private_dir(
            path.parent()
                .ok_or_else(|| anyhow::anyhow!("database needs parent"))?,
        )?;
        let _opening = database_lock(path, ".open.lock", true, true)?;
        let database_lock = database_lock(path, ".lock", false, true)?;
        let mut c = encrypted_connection(path, key, OpenFlags::default())?;
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
        apply_migrations(&mut c)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }

        Ok(Arc::new(Self {
            connection: Mutex::new(c),
            _database_lock: database_lock,
            key: key.clone(),
            path: path.to_owned(),
            _temporary_profile: None,
        }))
    }

    pub async fn export(self: &Arc<Self>, path: PathBuf) -> Result<()> {
        let path = std::path::absolute(path)?;
        if path.try_exists()? || path == std::path::absolute(&self.path)? {
            bail!("snapshot destination already exists; choose a new path")
        }

        let key = self.key.clone();
        self.call(move |source| {
            let staged =
                tempfile::NamedTempFile::new_in(path.parent().context("snapshot needs parent")?)?;
            export_attached(source, staged.path(), &key)?;
            let snapshot =
                encrypted_connection(staged.path(), &key, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            validate_database(&snapshot, true)?;
            drop(snapshot);
            staged.as_file().sync_all()?;
            staged
                .persist_noclobber(&path)
                .map_err(|error| error.error)?;

            sync_parent(&path)
        })
        .await
    }

    pub fn import(
        destination: &Path,
        source: &Path,
        key: &auth::DatabaseKey,
        overwrite: bool,
    ) -> Result<()> {
        let snapshot = encrypted_connection(source, key, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        validate_database(&snapshot, true)?;
        drop(snapshot);
        private_dir(destination.parent().context("inbox needs parent")?)?;
        let _opening = database_lock(destination, ".open.lock", true, true)?;
        let _replacing = database_lock(destination, ".lock", true, false)?;
        if destination.try_exists()? && !overwrite {
            bail!("inbox already exists; replacement must be explicit")
        }

        let staged =
            tempfile::NamedTempFile::new_in(destination.parent().context("inbox needs parent")?)?;
        let mut target = encrypted_connection(staged.path(), key, OpenFlags::default())?;
        let sql_key = key.sqlcipher_key();
        target
            .execute(
                "ATTACH DATABASE ?1 AS ahrara_import KEY ?2",
                params![database_uri(source, "ro")?, sql_key.as_str()],
            )
            .map_err(|_| anyhow::anyhow!("cannot read encrypted inbox snapshot"))?;
        target.pragma_update(None, "synchronous", "FULL")?;
        let tx = target.transaction()?;
        let version: i64 =
            tx.pragma_query_value(Some("ahrara_import"), "user_version", |r| r.get(0))?;
        tx.execute_batch("SELECT sqlcipher_export('main','ahrara_import');")?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
        target.execute_batch("DETACH DATABASE ahrara_import;")?;
        validate_database(&target, true)?;
        drop(target);
        if destination.try_exists()? && destination.metadata()?.len() > 0 {
            let current = encrypted_connection(destination, key, OpenFlags::default())?;
            current.busy_timeout(Duration::from_secs(5))?;
            let checkpoint: i64 =
                current.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))?;
            let mode: String =
                current.pragma_update_and_check(None, "journal_mode", "DELETE", |r| r.get(0))?;
            if checkpoint != 0 || mode != "delete" {
                bail!("current inbox WAL is in use; replacement cancelled")
            }

            drop(current);
        }

        staged.as_file().sync_all()?;
        if overwrite {
            staged.persist(destination).map_err(|error| error.error)?;
        } else {
            staged
                .persist_noclobber(destination)
                .map_err(|error| error.error)?;
        }

        sync_parent(destination)
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

    /// Registers an already verified alias without generating a new address.
    pub(crate) async fn restore_address(
        self: &Arc<Self>,
        fingerprint: String,
        address: String,
    ) -> Result<Address> {
        let (route, alias, address) = auth::decode_address(&address)?;
        self.call(move |c| {
            let tx = c.transaction()?;
            tx.execute(
                "INSERT INTO addresses(identity_fingerprint,route_id,alias_id,address,created_at) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(address) DO NOTHING",
                params![fingerprint, &route[..], &alias[..], address, Utc::now().to_rfc3339()],
            )?;
            tx.execute(
                "UPDATE addresses SET enabled=1 WHERE address=?1 AND identity_fingerprint=?2",
                params![address, fingerprint]
            )?;
            let saved = find_address(&tx, &fingerprint, &address)?;
            tx.commit()?;
            Ok(saved)
        }).await
    }

    pub(crate) async fn record_consumed(self: &Arc<Self>, id: i64) -> Result<bool> {
        self.call(move |c| {
            Ok(c.execute(
                "INSERT OR IGNORE INTO sdk_receipts(message_id) SELECT id FROM messages WHERE id=?1",
                [id]
            )? > 0)
        }).await
    }

    pub(crate) async fn delete_for_inbox(
        self: &Arc<Self>,
        fingerprint: String,
        recipient: String,
        id: Option<i64>,
    ) -> Result<usize> {
        self.call(move |c| {
            Ok(c.execute(
                "DELETE FROM messages WHERE identity_fingerprint=?1 AND recipient=?2 COLLATE BINARY AND (?3 IS NULL OR id=?3)",
                params![fingerprint, recipient, id]
            )?)
        }).await
    }

    pub async fn create_address_with(
        self: &Arc<Self>,
        fingerprint: String,
        route: [u8; 16],
        label: Option<String>,
        mut generate: impl FnMut() -> [u8; 8] + Send + 'static,
    ) -> Result<Address> {
        if label.as_ref().is_some_and(|s| s.len() > 256) {
            bail!("invalid_argument: label must not exceed 256 UTF-8 bytes")
        }

        self.call(move |c| {
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let custom = read_custom_domain(&tx, &fingerprint)?;
            if let Some(domain) = &custom {
                let destination = find_address(&tx, &fingerprint, &domain.forwarding_address)?;
                if !destination.enabled {
                    bail!("invalid_argument: custom domain forwarding destination is disabled")
                }
            }

            loop {
                let alias = generate();
                let address = auth::address(&route, &alias);
                let public = custom.as_ref().map(|d| {
                    format!("{}@{}", address.split_once('@').unwrap().0, d.domain)
                });
                let forwarding = custom.as_ref().map(|d| d.forwarding_address.as_str());

                let result = tx.execute(
                    "INSERT INTO addresses(identity_fingerprint,route_id,alias_id,address,label,created_at,public_address,forwarding_address) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![
                        fingerprint,
                        &route[..],
                        &alias[..],
                        address,
                        label,
                        Utc::now().to_rfc3339(),
                        public,
                        forwarding
                    ]
                );

                match result {
                    Ok(_) => {
                        let saved = tx.query_row(
                            "SELECT * FROM addresses WHERE id=?1",
                            [tx.last_insert_rowid()],
                            address_row,
                        )?;
                        tx.commit()?;

                        return Ok(saved);
                    },
                    Err(rusqlite::Error::SqliteFailure(e, _))
                        if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE => continue,
                    Err(e) => return Err(e.into()),
                }
            }
        })
        .await
    }

    pub async fn custom_domain(
        self: &Arc<Self>,
        fingerprint: String,
    ) -> Result<Option<CustomDomain>> {
        self.call(move |c| read_custom_domain(c, &fingerprint))
            .await
    }

    pub async fn set_custom_domain(
        self: &Arc<Self>,
        fingerprint: String,
        domain: String,
        destination: String,
    ) -> Result<CustomDomain> {
        let domain = normalize_domain(&domain)?;
        self.call(move |c| {
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let destination = find_address(&tx, &fingerprint, &destination)?;
            if !destination.enabled {
                bail!("forwarding destination must be an enabled local Ahrara address")
            }

            let value = CustomDomain {
                domain,
                forwarding_address: destination.routing_address,
            };

            tx.execute(
                "INSERT INTO custom_domains(identity_fingerprint,domain,forwarding_address) VALUES(?1,?2,?3) ON CONFLICT(identity_fingerprint) DO UPDATE SET domain=excluded.domain,forwarding_address=excluded.forwarding_address",
                params![fingerprint, value.domain, value.forwarding_address]
            )?;
            tx.commit()?;

            Ok(value)
        }).await
    }

    pub async fn remove_custom_domain(self: &Arc<Self>, fingerprint: String) -> Result<()> {
        self.call(move |c| {
            c.execute(
                "DELETE FROM custom_domains WHERE identity_fingerprint=?1",
                [fingerprint],
            )?;

            Ok(())
        })
        .await
    }

    pub async fn routing_filter(self: &Arc<Self>, address: String) -> Result<String> {
        self.call(move |c| routing_filter(c, &address)).await
    }

    pub async fn addresses(
        self: &Arc<Self>,
        fingerprint: String,
        after: i64,
        limit: u32,
        enabled_only: bool,
    ) -> Result<Vec<Address>> {
        if after < 0 || limit == 0 || limit > 100 {
            bail!("invalid_argument: invalid pagination: limit 1..100, after_id >= 0")
        }

        self.call(move |c| {
            let enabled = if enabled_only { " AND enabled=1" } else { "" };
            let mut q = c.prepare_cached(&format!("SELECT * FROM addresses WHERE identity_fingerprint=?1 AND id>?2{enabled} ORDER BY id LIMIT ?3"))?;
            Ok(q.query_map(params![fingerprint,after,limit],address_row)?.collect::<rusqlite::Result<_>>()?)
        }).await
    }

    pub async fn latest_enabled_address(
        self: &Arc<Self>,
        fingerprint: String,
    ) -> Result<Option<Address>> {
        self.call(move |c| {
            Ok(c.prepare_cached("SELECT * FROM addresses WHERE identity_fingerprint=?1 AND enabled=1 ORDER BY id DESC LIMIT 1")?
                .query_row([fingerprint], address_row).optional()?)
        }).await
    }

    pub async fn address_count(
        self: &Arc<Self>,
        fingerprint: String,
        enabled_only: bool,
    ) -> Result<i64> {
        self.call(move |c| {
            let enabled = if enabled_only { " AND enabled=1" } else { "" };
            Ok(c.prepare_cached(&format!(
                "SELECT count(*) FROM addresses WHERE identity_fingerprint=?1{enabled}"
            ))?
            .query_row([fingerprint], |r| r.get(0))?)
        })
        .await
    }

    pub async fn get_address(self: &Arc<Self>, fingerprint: String, id: String) -> Result<Address> {
        self.call(move |c| Ok(find_address(c, &fingerprint, &id)?))
            .await
    }

    pub async fn set_address(
        self: &Arc<Self>,
        fingerprint: String,
        id: String,
        enabled: Option<bool>,
    ) -> Result<Address> {
        self.call(move |c| {
            let tx = c.transaction()?;
            let mut a = find_address(&tx, &fingerprint, &id)?;
            if let Some(value) = enabled {
                tx.execute(
                    "UPDATE addresses SET enabled=?1 WHERE id=?2",
                    params![value, a.id],
                )?;

                a.enabled = value;
            } else {
                tx.execute("DELETE FROM addresses WHERE id=?1", [a.id])?;
            }

            tx.commit()?;
            Ok(a)
        })
        .await
    }

    pub async fn accepts(self: &Arc<Self>, fingerprint: String, recipient: String) -> Result<bool> {
        self.call(move |c| Ok(c.query_row(
            "SELECT EXISTS(SELECT 1 FROM addresses WHERE identity_fingerprint=?1 AND address=?2 COLLATE BINARY AND enabled=1)",
            params![fingerprint,recipient],
            |r|r.get(0)
        )?)).await
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
            let headers = mailparse::parse_headers(&raw).map(|x| x.0).unwrap_or_default();
            let header = |name| {
                headers.get_first_value(name).unwrap_or_default()
                    .chars().take(4096).collect::<String>()
            };
            let from = header("From");
            let to = header("To");
            let subject = header("Subject");
            let message_id = headers.get_first_value("Message-ID")
                .map(|s| s.chars().take(4096).collect::<String>());
            let received = Utc::now().to_rfc3339();

            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let mut saved = Vec::new();

            for recipient in recipients {
                let changed = tx.execute(
                    "INSERT INTO messages(transfer_id,identity_fingerprint,recipient,envelope_sender,received_at,from_header,to_header,subject,message_id,size_bytes,raw_eml) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11) ON CONFLICT(transfer_id,recipient) DO NOTHING",
                    params![
                        transfer,
                        fingerprint,
                        recipient,
                        sender,
                        received,
                        from,
                        to,
                        subject,
                        message_id,
                        raw.len() as i64,
                        &raw
                    ]
                )?;
                if changed == 1 {
                    saved.push(tx.query_row(
                        &format!("SELECT {EMAIL_COLUMNS} FROM messages WHERE id=?1"),
                        [tx.last_insert_rowid()],
                        email_row,
                    )?);
                }
            }

            tx.commit()?;
            Ok(saved)
        })
        .await
    }

    pub async fn list(
        self: &Arc<Self>,
        filters: Filters,
        identity: Option<String>,
    ) -> Result<Vec<Email>> {
        self.query_emails(filters, identity, None, None, false, false)
            .await
    }

    pub(crate) async fn list_for_wait(
        self: &Arc<Self>,
        filters: Filters,
        identity: String,
        unconsumed_only: bool,
    ) -> Result<Vec<Email>> {
        self.query_emails(filters, Some(identity), None, None, false, unconsumed_only)
            .await
    }

    pub async fn inbox_page(
        self: &Arc<Self>,
        filters: Filters,
        identity: String,
        before: Option<i64>,
        search: Option<String>,
    ) -> Result<Vec<Email>> {
        self.query_emails(filters, Some(identity), before, search, true, false)
            .await
    }

    async fn query_emails(
        self: &Arc<Self>,
        filters: Filters,
        identity: Option<String>,
        before: Option<i64>,
        search: Option<String>,
        newest: bool,
        unconsumed_only: bool,
    ) -> Result<Vec<Email>> {
        filters.validate()?;
        self.call(move |c| {
            let address = filters.address.map(|s| routing_filter(c, &s)).transpose()?;
            // Optional OR predicates prevent SQLite from seeking the recipient
            // index. Build only fixed clauses; all user values remain bound.
            let mut sql = format!("SELECT {EMAIL_COLUMNS} FROM messages WHERE id>?");
            let mut values = vec![Value::Integer(filters.after_id.unwrap_or(0))];

            for (clause, value) in [
                ("recipient=? COLLATE BINARY", address),
                (
                    "instr(lower(from_header || ' ' || envelope_sender),lower(?))>0",
                    filters.from_contains,
                ),
                ("instr(lower(subject),lower(?))>0", filters.subject_contains),
                (
                    "instr(lower(to_header || ' ' || recipient),lower(?))>0",
                    filters.to_contains,
                ),
                ("identity_fingerprint=?", identity),
            ] {
                if let Some(value) = value {
                    sql.push_str(" AND ");
                    sql.push_str(clause);
                    values.push(Value::Text(value));
                }
            }
            if let Some(before) = before {
                sql.push_str(" AND id<?"); values.push(Value::Integer(before));
            }
            if let Some(search) = search.filter(|s| !s.is_empty()) {
                sql.push_str(" AND instr(lower(from_header || ' ' || envelope_sender || ' ' || subject),lower(?))>0");
                values.push(Value::Text(search));
            }
            if unconsumed_only {
                // Keep receipt state in the encrypted database; no per-process
                // history copy or consumed MIME parsing is needed on each wait.
                sql.push_str(" AND NOT EXISTS (SELECT 1 FROM sdk_receipts WHERE message_id=messages.id)");
            }

            sql.push_str(if newest { " ORDER BY id DESC LIMIT ?" } else { " ORDER BY id LIMIT ?" });
            values.push(Value::Integer(filters.limit.unwrap_or(20).into()));
            let mut q = c.prepare_cached(&sql)?;
            Ok(q.query_map(params_from_iter(values), email_row)?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// Ten-message keyset pages for the interactive inbox, always newest first.
    /// `newer` fetches the nearest page above the cursor instead of below it.
    pub async fn email_page(
        self: &Arc<Self>,
        fingerprint: String,
        cursor: Option<i64>,
        newer: bool,
    ) -> Result<Vec<Email>> {
        self.call(move |c| {
            let (comparison, order) = if newer { (">", "ASC") } else { ("<", "DESC") };
            let boundary = cursor.unwrap_or(if newer { 0 } else { i64::MAX });
            let mut q = c.prepare_cached(&format!(
                "SELECT {EMAIL_COLUMNS} FROM messages WHERE identity_fingerprint=?1 AND id{comparison}?2 ORDER BY id {order} LIMIT 10"
            ))?;
            let mut rows = q.query_map(params![fingerprint, boundary], email_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if newer { rows.reverse(); }

            Ok(rows)
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
        self.call(move |c| {
            Ok(match id {
                Some(id) => c.execute("DELETE FROM messages WHERE id=?1", [id])?,
                None => c.execute("DELETE FROM messages", [])?,
            })
        })
        .await
    }

    pub async fn stats(self: &Arc<Self>) -> Result<(i64, Option<String>, i64)> {
        self.call(|c| {
            Ok(c.query_row(
                // Separate aggregates let SQLite use its count and min/max
                // optimizations, while one statement retains a single snapshot.
                "SELECT (SELECT count(*) FROM messages),(SELECT max(received_at) FROM messages),(SELECT coalesce(max(id),0) FROM messages)",
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

#[cfg(test)]
mod encryption_tests {
    use super::*;
    use crate::{auth::Identity, config::Config};

    fn config(dir: &Path) -> Config {
        Config {
            database_path: dir.join("inbox.db"),
            identity_path: dir.join("identity.key"),
            api_token_path: dir.join("api.token"),
            ..Config::default()
        }
    }

    #[test]
    fn invalid_email_filters_are_argument_errors() {
        for filters in [
            Filters {
                limit: Some(0),
                ..Default::default()
            },
            Filters {
                after_id: Some(-1),
                ..Default::default()
            },
            Filters {
                address: Some("not-an-address".into()),
                ..Default::default()
            },
        ] {
            let error = filters.validate().unwrap_err().to_string();

            assert!(error.starts_with("invalid_argument:"), "{error}");
        }
    }

    #[test]
    fn new_database_cannot_be_read_without_its_key() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let (identity, _) = Identity::load_or_create(&cfg).unwrap();
        let storage = Storage::open(&cfg.database_path, &identity.database_key()).unwrap();
        drop(storage);
        let without_key = Connection::open(&cfg.database_path).unwrap();

        assert!(
            without_key
                .query_row("SELECT count(*) FROM sqlite_schema", [], |r| r
                    .get::<_, i64>(0))
                .is_err(),
            "new inbox must be encrypted and reject a connection without its key"
        );
    }

    #[test]
    fn importing_another_identity_refuses_an_existing_database() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let cfg = config(first.path());
        let (original, _) = Identity::load_or_create(&cfg).unwrap();
        let (replacement, _) = Identity::load_or_create(&config(second.path())).unwrap();
        plain_fixture(&cfg.database_path);

        assert!(
            Identity::import(&replacement.export(), &cfg).is_err(),
            "replacing Identity must not orphan an existing encrypted inbox"
        );

        assert_eq!(
            Identity::load(&cfg).unwrap().public_key(),
            original.public_key()
        );
    }

    #[test]
    fn regenerating_identity_refuses_an_existing_database() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let (original, _) = Identity::load_or_create(&cfg).unwrap();
        plain_fixture(&cfg.database_path);

        assert!(
            Identity::regenerate(&cfg).is_err(),
            "generating a new Identity must not orphan an existing encrypted inbox"
        );

        assert_eq!(
            Identity::load(&cfg).unwrap().public_key(),
            original.public_key()
        );
    }

    fn plain_fixture(path: &Path) {
        Connection::open(path)
            .unwrap()
            .execute_batch("CREATE TABLE fixture(value);")
            .unwrap();
    }

    fn encrypted_fixture(path: &Path) {
        let connection = Connection::open(path).unwrap();
        // Synthetic database key, independent of every fixture Identity seed.
        connection
            .pragma_update(
                None,
                "key",
                "x'1111111111111111111111111111111111111111111111111111111111111111'",
            )
            .unwrap();

        connection
            .execute_batch("CREATE TABLE fixture(value); INSERT INTO fixture VALUES('synthetic');")
            .unwrap();
    }

    #[test]
    fn importing_identity_refuses_an_existing_encrypted_database() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let cfg = config(first.path());
        let (original, _) = Identity::load_or_create(&cfg).unwrap();
        let (replacement, _) = Identity::load_or_create(&config(second.path())).unwrap();
        encrypted_fixture(&cfg.database_path);

        assert!(Identity::import(&replacement.export(), &cfg).is_err());
        assert_eq!(
            Identity::load(&cfg).unwrap().public_key(),
            original.public_key()
        );
    }

    #[test]
    fn regenerating_identity_refuses_an_existing_encrypted_database() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let (original, _) = Identity::load_or_create(&cfg).unwrap();
        encrypted_fixture(&cfg.database_path);

        assert!(Identity::regenerate(&cfg).is_err());
        assert_eq!(
            Identity::load(&cfg).unwrap().public_key(),
            original.public_key()
        );
    }

    #[test]
    fn missing_identity_does_not_generate_over_an_existing_inbox() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        plain_fixture(&cfg.database_path);

        assert!(
            Identity::load_or_create(&cfg).is_err(),
            "missing Identity requires recovery before opening an existing inbox"
        );

        assert!(!cfg.identity_path.exists());
    }

    #[test]
    fn recovery_derives_the_same_separate_database_key() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let (identity, _) = Identity::load_or_create(&cfg).unwrap();
        let restored = Identity::from_recovery(&identity.export()).unwrap();

        assert!(
            identity.database_key().sqlcipher_key().as_str()
                == restored.database_key().sqlcipher_key().as_str()
        );
        let raw_seed = zeroize::Zeroizing::new(std::fs::read(&cfg.identity_path).unwrap());
        let encoded_seed = zeroize::Zeroizing::new(format!("x'{}'", hex::encode(&*raw_seed)));

        assert!(identity.database_key().sqlcipher_key().as_str() != encoded_seed.as_str());
        let (other, _) =
            Identity::load_or_create(&config(tempfile::tempdir().unwrap().path())).unwrap();

        assert!(
            identity.database_key().sqlcipher_key().as_str()
                != other.database_key().sqlcipher_key().as_str()
        );
    }

    #[test]
    fn a_wrong_key_does_not_modify_an_existing_encrypted_inbox() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let cfg = config(first.path());
        let (identity, _) = Identity::load_or_create(&cfg).unwrap();
        let (other, _) = Identity::load_or_create(&config(second.path())).unwrap();
        drop(Storage::open(&cfg.database_path, &identity.database_key()).unwrap());
        let before = std::fs::read(&cfg.database_path).unwrap();

        assert!(Storage::open(&cfg.database_path, &other.database_key()).is_err());
        assert!(std::fs::read(&cfg.database_path).unwrap() == before);
        assert!(Storage::open(&cfg.database_path, &identity.database_key()).is_ok());
    }

    #[test]
    fn registration_is_noclobber_and_checks_existing_ciphertext_before_installing() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let cfg = config(first.path());
        let (identity, _) = Identity::load_or_create(&cfg).unwrap();
        let (wrong, _) = Identity::load_or_create(&config(second.path())).unwrap();
        drop(Storage::open(&cfg.database_path, &identity.database_key()).unwrap());

        assert!(Identity::install_new(&identity.export(), &cfg).is_err());
        std::fs::remove_file(&cfg.identity_path).unwrap();

        assert!(Identity::install_new(&wrong.export(), &cfg).is_err());
        assert!(!cfg.identity_path.exists());
        let installed = Identity::install_new(&identity.export(), &cfg).unwrap();

        assert_eq!(installed.public_key(), identity.public_key());
        assert!(Storage::open(&cfg.database_path, &installed.database_key()).is_ok());
    }

    #[test]
    fn plaintext_inbox_is_rejected_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let (identity, _) = Identity::load_or_create(&cfg).unwrap();
        plain_fixture(&cfg.database_path);
        let before = std::fs::read(&cfg.database_path).unwrap();
        let error = Storage::open(&cfg.database_path, &identity.database_key())
            .err()
            .unwrap();

        assert_eq!(
            error.to_string(),
            "incorrect inbox key or damaged encrypted database"
        );
        assert!(std::fs::read(&cfg.database_path).unwrap() == before);
    }

    #[tokio::test]
    async fn query_index_migration_preserves_existing_inbox() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let (identity, _) = Identity::load_or_create(&cfg).unwrap();
        let key = identity.database_key();
        {
            let c = encrypted_connection(&cfg.database_path, &key, OpenFlags::default()).unwrap();
            c.execute_batch(MIGRATIONS[0]).unwrap();
            c.execute_batch(MIGRATIONS[1]).unwrap();
            c.pragma_update(None, "user_version", 2).unwrap();
            c.execute(
                "INSERT INTO messages(transfer_id,identity_fingerprint,recipient,envelope_sender,received_at,from_header,to_header,subject,size_bytes,raw_eml) VALUES('existing','fp','recipient','sender','timestamp','','','',3,?1)",
                [b"\0\r\n".as_slice()]
            ).unwrap();
            c.execute(
                "INSERT INTO addresses(identity_fingerprint,route_id,alias_id,address,created_at) VALUES('fp',zeroblob(16),zeroblob(8),'existing','timestamp')",
                []
            ).unwrap();
        }

        for _ in 0..2 {
            let storage = Storage::open(&cfg.database_path, &key).unwrap();
            assert_eq!(storage.schema_version().await.unwrap(), 5);
            assert_eq!(
                storage.stats().await.unwrap(),
                (1, Some("timestamp".into()), 1)
            );
            assert_eq!(storage.email(1).await.unwrap().1, b"\0\r\n");
            assert_eq!(storage.address_count("fp".into(), true).await.unwrap(), 1);
            assert_eq!(
                storage
                    .get_address("fp".into(), "1".into())
                    .await
                    .unwrap()
                    .address,
                "existing"
            );
        }
    }

    #[tokio::test]
    async fn encrypted_snapshot_roundtrip_preserves_wal_metadata_and_exact_email_bytes() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let cfg = config(first.path());
        let (identity, _) = Identity::load_or_create(&cfg).unwrap();
        let key = identity.database_key();
        let storage = Storage::open(&cfg.database_path, &key).unwrap();
        let fingerprint = identity.fingerprint();
        let address = storage
            .create_address(
                fingerprint.clone(),
                identity.route(),
                Some("snapshot label".into()),
            )
            .await
            .unwrap();
        storage
            .set_custom_domain(
                fingerprint.clone(),
                "example.test".into(),
                address.id.to_string(),
            )
            .await
            .unwrap();
        let raw = b"Subject: synthetic-sqlcipher-marker\r\nContent-Type: application/octet-stream\r\n\r\n\0\xFFprivate-synthetic-email\r\n".to_vec();
        let email = storage
            .insert(
                "snapshot-transfer".into(),
                fingerprint.clone(),
                vec![address.address.clone()],
                "sender@example.test".into(),
                raw.clone(),
            )
            .await
            .unwrap();

        for suffix in ["", "-wal"] {
            let name = format!("{}{suffix}", cfg.database_path.display());
            if let Ok(bytes) = std::fs::read(name) {
                assert!(
                    !bytes
                        .windows(b"synthetic-sqlcipher-marker".len())
                        .any(|part| part == b"synthetic-sqlcipher-marker")
                );
            }
        }

        let snapshot = first.path().join("snapshot ' unicode-é %#.db");
        storage.export(snapshot.clone()).await.unwrap();

        assert!(
            !std::fs::read(&snapshot)
                .unwrap()
                .starts_with(b"SQLite format 3\0")
        );
        assert!(storage.export(snapshot.clone()).await.is_err());
        let snapshot_bytes = std::fs::read(&snapshot).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                snapshot.metadata().unwrap().permissions().mode() & 0o777,
                0o600
            );

            std::fs::set_permissions(&snapshot, std::fs::Permissions::from_mode(0o400)).unwrap();
        }
        let target = config(second.path());
        Storage::import(&target.database_path, &snapshot, &key, false).unwrap();

        assert!(std::fs::read(&snapshot).unwrap() == snapshot_bytes);
        let restored = Identity::install_new(&identity.export(), &target).unwrap();
        let reopened = Storage::open(&target.database_path, &restored.database_key()).unwrap();

        assert_eq!(reopened.email(email[0].id).await.unwrap().1, raw);
        assert_eq!(reopened.schema_version().await.unwrap(), 5);
        assert_eq!(
            reopened
                .get_address(fingerprint.clone(), address.id.to_string())
                .await
                .unwrap()
                .label
                .as_deref(),
            Some("snapshot label")
        );
        assert_eq!(
            reopened
                .custom_domain(fingerprint)
                .await
                .unwrap()
                .unwrap()
                .domain,
            "example.test"
        );
        assert!(Storage::import(&target.database_path, &snapshot, &key, true).is_err());
        drop(reopened);

        assert!(Storage::import(&target.database_path, &snapshot, &key, false).is_err());
        Storage::import(&target.database_path, &snapshot, &key, true).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                snapshot.metadata().unwrap().permissions().mode() & 0o777,
                0o400
            );
        }
    }

    #[tokio::test]
    async fn snapshot_missing_required_index_preserves_the_current_inbox() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let (identity, _) = Identity::load_or_create(&cfg).unwrap();
        let key = identity.database_key();
        let storage = Storage::open(&cfg.database_path, &key).unwrap();
        let snapshot = dir.path().join("missing-index.db");
        storage.export(snapshot.clone()).await.unwrap();
        let raw = b"Subject: preserve destination\r\n\r\nsynthetic email".to_vec();
        let emails = storage
            .insert(
                "destination-transfer".into(),
                identity.fingerprint(),
                vec!["recipient@example.test".into()],
                "sender@example.test".into(),
                raw.clone(),
            )
            .await
            .unwrap();
        drop(storage);
        let before = std::fs::read(&cfg.database_path).unwrap();
        let c = encrypted_connection(&snapshot, &key, OpenFlags::default()).unwrap();
        c.execute_batch("DROP INDEX addresses_identity_id").unwrap();
        drop(c);

        assert!(
            Storage::import(&cfg.database_path, &snapshot, &key, true).is_err(),
            "a snapshot missing a required index must be rejected"
        );
        assert!(std::fs::read(&cfg.database_path).unwrap() == before);
        let reopened = Storage::open(&cfg.database_path, &key).unwrap();

        assert_eq!(reopened.email(emails[0].id).await.unwrap().1, raw);
    }

    #[tokio::test]
    async fn invalid_imports_preserve_the_current_inbox() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let (identity, _) = Identity::load_or_create(&cfg).unwrap();
        let key = identity.database_key();
        let storage = Storage::open(&cfg.database_path, &key).unwrap();
        let snapshot = dir.path().join("snapshot.db");
        storage.export(snapshot.clone()).await.unwrap();
        drop(storage);
        let before = std::fs::read(&cfg.database_path).unwrap();
        let malformed = dir.path().join("corrupt.db");
        std::fs::write(&malformed, b"not a database").unwrap();
        let plaintext = dir.path().join("plaintext.db");
        plain_fixture(&plaintext);
        let newer = dir.path().join("newer.db");
        std::fs::copy(&snapshot, &newer).unwrap();
        let c = encrypted_connection(&newer, &key, OpenFlags::default()).unwrap();
        c.pragma_update(None, "user_version", 99).unwrap();
        drop(c);

        for invalid in [&malformed, &plaintext, &newer] {
            assert!(Storage::import(&cfg.database_path, invalid, &key, true).is_err());
            assert!(std::fs::read(&cfg.database_path).unwrap() == before);
        }

        assert!(Storage::open(&cfg.database_path, &key).is_ok());
    }
}
