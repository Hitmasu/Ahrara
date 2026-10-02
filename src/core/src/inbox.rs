//! Isolated, disposable inboxes embedded directly in a caller's process.
use crate::{
    auth::{self, Identity},
    config::{Config, private_dir},
    core::Core,
    filter::EmailFilter,
    mail::{Attachment, HtmlEmail, ParsedEmail},
    query::{CompiledQuery, Query, QueryMatches, RegexMatch},
    storage::Filters,
};
use anyhow::{Context, Result, bail};
use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroize;

/// Relay connection and optional persistent profile. No user CLI configuration is read.
#[derive(Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct InboxOptions {
    /// Authenticated relay endpoint; defaults to the public Ahrara relay.
    pub server_url: String,
    /// Verified public relay key; never obtained from the connection itself.
    pub server_public_key: Option<String>,
    /// Additional trusted CA for a private relay. System/public roots remain enabled.
    pub tls_ca_path: Option<PathBuf>,
    /// Connection setup limit in milliseconds (1–300000), independent of receive timeouts.
    pub connect_timeout_ms: u64,
    /// Absolute directory retaining the identity, encrypted history and receipt state.
    /// Omit to use an isolated temporary profile removed after shutdown.
    pub profile_path: Option<PathBuf>,
    /// Restores an address and its owning identity; never log these credentials.
    pub credentials: Option<InboxCredentials>,
    /// Imports an identity without requiring a known address. Never log this key.
    /// Cannot be combined with `credentials`; cannot replace an existing identity.
    pub recovery_key: Option<String>,
    /// An existing Ahrara address belonging to the imported or stored identity.
    /// Omit to reuse the profile's latest enabled address or generate one.
    pub address: Option<String>,
    /// Admission filter: rejected deliveries are acknowledged and never stored.
    /// Also restricts reads of history retained by an existing profile.
    pub filter: Option<EmailFilter>,
}

impl Default for InboxOptions {
    fn default() -> Self {
        Self {
            server_url: "wss://ahrara.dev".into(),
            server_public_key: option_env!("AHRARA_SERVER_PUBLIC_KEY")
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            tls_ca_path: None,
            connect_timeout_ms: 20_000,
            profile_path: None,
            credentials: None,
            recovery_key: None,
            address: None,
            filter: None,
        }
    }
}

/// Substring filters and an optional total receive deadline.
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WaitOptions {
    /// Positive milliseconds, or `None` to wait indefinitely until closed/cancelled.
    pub timeout_ms: Option<u64>,
    /// Case-insensitive subject substring; omitted means any subject.
    pub subject: Option<String>,
    /// Case-insensitive sender substring; omitted means any sender.
    pub from: Option<String>,
    /// Additional predicates, applied after the inbox filter. Rejected messages
    /// remain available to subsequent calls with a different method filter.
    pub filter: Option<EmailFilter>,
}

/// Secret capability to receive mail for an address. Debug output is redacted.
/// Exporting/importing credentials does not copy the local message history.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InboxCredentials {
    /// Canonical address owned by this identity.
    pub address: String,
    /// Identity recovery key. Store in a secret store; never include in test logs.
    pub recovery_key: String,
}

impl fmt::Debug for InboxCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InboxCredentials { recovery_key: [REDACTED], .. }")
    }
}

impl Drop for InboxCredentials {
    fn drop(&mut self) {
        self.recovery_key.zeroize();
    }
}

/// A page of stored metadata. Listing never changes receipt state.
#[derive(Serialize)]
pub struct EmailPage {
    /// Messages in increasing local ID order; use `get_email` for bodies and attachments.
    pub emails: Vec<crate::storage::Email>,
    /// Pass as `Filters.after_id` to continue; `None` indicates the final page.
    pub next_cursor: Option<i64>,
}

type PendingReceive = (i64, tokio::task::JoinHandle<Result<Option<Received>>>);

// A bounded continuation for short native polling calls. No message is
// consumed until its retained parse result is returned successfully.
#[derive(Default)]
struct ReceiveState {
    key: String,
    after_id: i64,
    pending: Option<PendingReceive>,
}

enum Received {
    Email(Box<ParsedEmail>),
    MailMessage(Box<crate::mail::MailMessage>),
    Query(QueryMatches),
}

#[derive(Clone)]
enum ReceiveKind {
    Email,
    MailMessage,
    Query(Arc<CompiledQuery>),
}

/// Owns an authenticated receiver and encrypted inbox. No daemon, UI or local
/// API is started. Close before reopening the same identity in another receiver.
pub struct Inbox {
    receiver: Arc<Core>,
    address: String,
    receive: Mutex<ReceiveState>,
    closed: CancellationToken,
}

impl Inbox {
    /// Returns only after the authenticated receiver is ready. Receipt runs
    /// continuously, including before the first `wait_for_email` call.
    pub async fn create(options: InboxOptions) -> Result<Self> {
        if let Some(filter) = &options.filter {
            filter.validate()?;
        }

        let filter = options.filter.clone();
        if !(1..=300_000).contains(&options.connect_timeout_ms) {
            bail!("invalid_argument: connect_timeout_ms must be 1..300000")
        }
        if options.server_public_key.is_none() {
            bail!(
                "invalid_argument: server_public_key is required; supply a verified relay public key"
            )
        }
        if options.credentials.is_some()
            && (options.recovery_key.is_some() || options.address.is_some())
        {
            bail!("invalid_argument: credentials cannot be combined with recovery_key or address")
        }

        let address = options
            .credentials
            .as_ref()
            .map(|value| value.address.clone())
            .or(options.address.clone());
        let connect_timeout = Duration::from_millis(options.connect_timeout_ms);
        let receiver = tokio::task::spawn_blocking(move || {
            let temporary = if options.profile_path.is_none() {
                Some(tempfile::Builder::new().prefix("ahrara-sdk-").tempdir()?)
            } else {
                None
            };
            let path = options
                .profile_path
                .as_deref()
                .unwrap_or_else(|| temporary.as_ref().unwrap().path());
            if !path.is_absolute() {
                bail!("invalid_argument: profile_path must be absolute")
            }

            private_dir(path)?;
            // Set every location explicitly: SDKs never read the user's profile.
            let config = Config {
                server_url: options.server_url,
                server_public_key: options.server_public_key,
                tls_ca_path: options.tls_ca_path,
                database_path: path.join("inbox.db"),
                identity_path: path.join("identity.key"),
                api_token_path: path.join("unused.token"),
                api_tls_cert_path: path.join("unused.pem"),
                api_tls_key_path: path.join("unused.key"),
                api_bind: "127.0.0.1:0".into(),
                api_https: false,
                mcp_auth: false,
                check_for_updates: false,
                smtp_dev_address: String::new(),
                domain: "ahrara.dev".into(),
            };
            config.validate()?;
            let recovery = options
                .credentials
                .as_ref()
                .map(|value| value.recovery_key.as_str())
                .or(options.recovery_key.as_deref());
            if let Some(recovery) = recovery {
                let imported = Identity::from_recovery(recovery)
                    .map_err(|_| anyhow::anyhow!("invalid_argument: invalid recovery key"))?;
                if config.identity_path.exists() {
                    if Identity::load(&config)?.public_key() != imported.public_key() {
                        bail!("invalid_argument: profile belongs to another identity")
                    }
                } else {
                    Identity::install_new(recovery, &config)?;
                }
            }

            match temporary {
                Some(directory) => Core::open_temporary(config, directory),
                None => Ok(Core::open(config)?.0),
            }
        })
        .await??;
        if let Some(filter) = filter {
            receiver
                .inbox_filter
                .set(filter)
                .map_err(|_| anyhow::anyhow!("internal_error: inbox filter already configured"))?;
        }

        let address = if let Some(address) = address {
            let (route, _, normalized) = auth::decode_address(&address)
                .map_err(|_| anyhow::anyhow!("invalid_argument: invalid inbox address"))?;
            if route != receiver.identity.route() {
                bail!("invalid_argument: address belongs to another identity")
            }

            receiver
                .storage
                .restore_address(receiver.identity.fingerprint(), normalized)
                .await?
                .routing_address
        } else if let Some(address) = receiver
            .storage
            .latest_enabled_address(receiver.identity.fingerprint())
            .await?
        {
            address.routing_address
        } else {
            receiver.create_address(None).await?.routing_address
        };
        let inbox = Self {
            receiver,
            address,
            receive: Mutex::new(ReceiveState::default()),
            closed: CancellationToken::new(),
        };
        let mut state = inbox.receiver.state.subscribe();
        inbox.receiver.start("sdk", false).await?;
        let ready = tokio::time::timeout(connect_timeout, async {
            loop {
                let current = state.borrow().clone();
                if current.connected {
                    return Ok(());
                }
                if current.state == "failed" {
                    bail!(
                        "connection_failed: {}",
                        current
                            .last_error
                            .unwrap_or_else(|| "receiver failed".into())
                    );
                }

                state
                    .changed()
                    .await
                    .context("connection_failed: receiver stopped")?;
            }
        })
        .await;

        match ready {
            Ok(Ok(())) => Ok(inbox),
            other => {
                let last_error = inbox.receiver.state.borrow().last_error.clone();
                inbox.close().await;
                match other {
                    Ok(Err(error)) => Err(error),
                    Err(_) => anyhow::bail!(
                        "connection_failed: connection timed out ({})",
                        last_error.unwrap_or_else(|| "handshake did not complete".into())
                    ),
                    Ok(Ok(())) => unreachable!(),
                }
            }
        }
    }

    /// The address ready to receive mail after successful creation.
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Waits for the next matching email, consuming each match once while
    /// preserving unmatched messages for later calls with different filters.
    /// Calls on the same inbox are serialized; timeouts include time waiting for
    /// that cursor. Dropping this future cancels only this wait, not reception.
    pub async fn wait_for_email(&self, options: WaitOptions) -> Result<Option<ParsedEmail>> {
        match self
            .receive(options, ReceiveKind::Email, "email", None)
            .await?
        {
            Some(Received::Email(email)) => Ok(Some(*email)),
            None => Ok(None),
            _ => unreachable!("receive key identifies the result type"),
        }
    }

    /// Receives a complete BCL-compatible MIME projection using the shared parser.
    /// Decoding completes before receipt is recorded; headers and bodies are not truncated.
    pub async fn wait_for_mail_message(
        &self,
        options: WaitOptions,
    ) -> Result<Option<crate::mail::MailMessage>> {
        match self
            .receive(options, ReceiveKind::MailMessage, "mail_message", None)
            .await?
        {
            Some(Received::MailMessage(email)) => Ok(Some(*email)),
            None => Ok(None),
            _ => unreachable!("receive key identifies the result type"),
        }
    }

    async fn receive(
        &self,
        options: WaitOptions,
        kind: ReceiveKind,
        query_key: &str,
        cancel: Option<&CancellationToken>,
    ) -> Result<Option<Received>> {
        let deadline = receive_deadline(options.timeout_ms)?;
        if let Some(filter) = &options.filter {
            filter.validate()?;
        }

        let key =
            serde_json::to_string(&(query_key, &options.subject, &options.from, &options.filter))?;

        loop {
            let (mut receive, result) = tokio::select! {
                biased;
                _ = self.closed.cancelled() => bail!("inbox_closed: inbox is closed"),
                _ = async { match cancel { Some(token) => token.cancelled().await, None => std::future::pending().await } } => bail!("cancelled: receive was cancelled"),
                _ = wait_until(deadline) => return Ok(None),
                result = async {
                    let mut receive = self.receive.lock().await;
                    if receive.key != key {
                        // Keep at most one parser per inbox, even if callers change
                        // predicates while a previous timed-out parse is running.
                        if let Some((_, job)) = receive.pending.as_mut() { let _ = job.await; }
                        receive.pending = None;
                        receive.key.clone_from(&key);
                        receive.after_id = 0;
                    }
                    let mut filters = Filters {
                        address: Some(self.address.clone()), after_id: Some(receive.after_id),
                        subject_contains: options.subject.clone(), from_contains: options.from.clone(),
                        ..Default::default()
                    };
                    loop {
                        if receive.pending.is_none() {
                        let remaining = deadline.map(|value| value.saturating_duration_since(tokio::time::Instant::now()));
                        let Some(email) = self.receiver.wait_email_metadata_for(filters.clone(), remaining, true).await? else {
                            return Ok::<_, anyhow::Error>((receive, None));
                        };
                        let id = email.id;
                        let (metadata, raw) = self.receiver.storage.email(id).await?;
                        let global_filter = self.receiver.inbox_filter.get().cloned();
                        let filter = options.filter.clone();
                        let kind = kind.clone();
                        receive.pending = Some((id, tokio::task::spawn_blocking(move || {
                            for filter in [&global_filter, &filter].into_iter().flatten() {
                                if !filter.matches(&raw, &metadata.recipient, &metadata.envelope_sender)? { return Ok(None); }
                            }
                            match kind {
                                ReceiveKind::Email => crate::mail::parse(metadata, &raw, 256 * 1024).map(|email| Some(Received::Email(Box::new(email)))),
                                ReceiveKind::MailMessage => crate::mail::mail_message(metadata, &raw).map(|email| Some(Received::MailMessage(Box::new(email)))),
                                ReceiveKind::Query(query) => query.evaluate(&metadata, &raw).map(|value| value.map(Received::Query)),
                            }
                        })));
                        }
                        // Borrow the task through await: a native polling timeout
                        // retains both work and scan progress for the next slice.
                        let (id, job) = receive.pending.as_mut().expect("parser is scheduled");
                        let id = *id;
                        let parsed = job.await;
                        receive.pending = None;
                        if let Some(message) = parsed?? {
                            return Ok((receive, Some((id, message))));
                        }
                        receive.after_id = id;
                        filters.after_id = Some(id);
                    }
                } => result?,
            };

            match result {
                Some((id, message)) => {
                    // Once selected, complete the receipt write instead of reporting a
                    // timeout after committing it. The message remains in history.
                    let exists = self.receiver.storage.record_consumed(id).await?;
                    receive.after_id = id;
                    if exists {
                        return Ok(Some(message));
                    }
                    // Delete/clear can run while a timed-out parser is retained.
                    // Never return an obsolete cached message after it was deleted.
                }
                None => return Ok(None),
            }
        }
    }

    /// Extracts from the first eligible email satisfying the query. Nonmatches
    /// are never consumed; invalid expressions fail before a message is read.
    /// `Query::all` selects one or every occurrence within this single email.
    pub async fn wait_for_query(
        &self,
        query: Query,
        options: WaitOptions,
    ) -> Result<Option<QueryMatches>> {
        let key = serde_json::to_string(&query)?;
        let query = Arc::new(CompiledQuery::compile(query)?);
        match self
            .receive(options, ReceiveKind::Query(query), &key, None)
            .await?
        {
            Some(Received::Query(found)) => Ok(Some(found)),
            None => Ok(None),
            _ => unreachable!("receive key identifies the result type"),
        }
    }

    /// Lazily extracts matches from successive emails for one total duration.
    /// Query `all` controls occurrences per email; cancellation leaves other mail available.
    pub fn wait_for_queries(
        &self,
        query: Query,
        options: WaitOptions,
        cancel: CancellationToken,
    ) -> impl Stream<Item = Result<QueryMatches>> + '_ {
        async_stream::try_stream! {
            let key = serde_json::to_string(&query)?;
            let query = Arc::new(CompiledQuery::compile(query)?);
            let deadline = receive_deadline(options.timeout_ms)?;
            loop {
                if deadline.is_some_and(|value| value <= tokio::time::Instant::now()) { break; }
                let mut next = options.clone();
                next.timeout_ms = deadline.map(|value| value.saturating_duration_since(tokio::time::Instant::now()).as_millis().max(1) as u64);
                let query = Arc::clone(&query);
                let found = self.receive(next, ReceiveKind::Query(query), &key, Some(&cancel)).await?;
                match found { Some(Received::Query(found)) => yield found, None => break,
                    _ => unreachable!("receive key identifies the result type") }
            }
        }
    }

    /// Returns the first regex match with numbered/named groups; searches decoded plain text.
    /// Use `wait_for_query` to select HTML, subject, raw source or regex flags.
    pub async fn wait_for_regex_match(
        &self,
        pattern: impl Into<String>,
        options: WaitOptions,
    ) -> Result<Option<RegexMatch>> {
        Ok(self
            .wait_for_query(
                Query::Regex {
                    pattern: pattern.into(),
                    source: Default::default(),
                    ignore_case: false,
                    multi_line: false,
                    dot_matches_new_line: false,
                    all: false,
                },
                options,
            )
            .await?
            .and_then(|found| found.matches.into_iter().next()))
    }

    /// Yields all regex occurrences across eligible messages during the total duration.
    pub fn wait_for_regex_matches(
        &self,
        pattern: impl Into<String>,
        options: WaitOptions,
        cancel: CancellationToken,
    ) -> impl Stream<Item = Result<RegexMatch>> + '_ {
        let query = Query::Regex {
            pattern: pattern.into(),
            source: Default::default(),
            ignore_case: false,
            multi_line: false,
            dot_matches_new_line: false,
            all: true,
        };

        async_stream::try_stream! {
            let deadline = receive_deadline(options.timeout_ms)?;
            let stream = self.wait_for_queries(query, options, cancel.clone());
            futures_util::pin_mut!(stream);
            while let Some(found) = stream.next().await {
                for value in found?.matches {
                    if cancel.is_cancelled() { Err(anyhow::anyhow!("cancelled: query stream was cancelled"))?; }
                    if deadline.is_some_and(|value| value <= tokio::time::Instant::now()) { return; }
                    yield value;
                }
            }
        }
    }

    /// Evaluates XPath 1.0 against normalized HTML and returns the first value.
    pub async fn wait_for_xpath(
        &self,
        expression: impl Into<String>,
        options: WaitOptions,
    ) -> Result<Option<String>> {
        Ok(self
            .wait_for_query(
                Query::XPath {
                    expression: expression.into(),
                    all: false,
                },
                options,
            )
            .await?
            .and_then(|found| found.values.into_iter().next()))
    }

    /// Yields XPath values across all eligible emails until duration/cancellation.
    pub fn wait_for_xpath_values(
        &self,
        expression: impl Into<String>,
        options: WaitOptions,
        cancel: CancellationToken,
    ) -> impl Stream<Item = Result<String>> + '_ {
        let query = Query::XPath {
            expression: expression.into(),
            all: true,
        };

        async_stream::try_stream! {
            let deadline = receive_deadline(options.timeout_ms)?;
            let stream = self.wait_for_queries(query, options, cancel.clone());
            futures_util::pin_mut!(stream);
            while let Some(found) = stream.next().await {
                for value in found?.values {
                    if cancel.is_cancelled() { Err(anyhow::anyhow!("cancelled: query stream was cancelled"))?; }
                    if deadline.is_some_and(|value| value <= tokio::time::Instant::now()) { return; }
                    yield value;
                }
            }
        }
    }

    /// Lazily receives matching messages until the total duration expires.
    /// `None` waits indefinitely. Cancellation yields a `cancelled` error; dropping
    /// the stream leaves the inbox connected. Consumer work counts toward duration.
    pub fn wait_for_emails(
        &self,
        options: WaitOptions,
        cancel: CancellationToken,
    ) -> impl Stream<Item = Result<ParsedEmail>> + '_ {
        async_stream::try_stream! {
            let deadline = receive_deadline(options.timeout_ms)?;
            loop {
                if deadline.is_some_and(|value| value <= tokio::time::Instant::now()) { break; }
                let mut next = options.clone();
                next.timeout_ms = deadline.map(|value| value.saturating_duration_since(tokio::time::Instant::now()).as_millis().max(1) as u64);
                let email = self.receive(next, ReceiveKind::Email, "email", Some(&cancel)).await?;
                match email { Some(Received::Email(email)) => yield *email, None => break,
                    _ => unreachable!("receive key identifies the result type") }
            }
        }
    }

    /// Invokes an awaited callback sequentially for each match. Callback failures
    /// stop this receiver and propagate; they do not close or delete the inbox.
    pub async fn receive_emails<F, Fut>(
        &self,
        options: WaitOptions,
        cancel: CancellationToken,
        mut handler: F,
    ) -> Result<()>
    where
        F: FnMut(ParsedEmail) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        let stream = self.wait_for_emails(options, cancel);
        futures_util::pin_mut!(stream);

        while let Some(email) = stream.next().await {
            handler(email?).await?;
        }

        Ok(())
    }

    /// Exports secret identity credentials. History remains in the profile directory.
    pub fn export_credentials(&self) -> Result<InboxCredentials> {
        self.require_open()?;
        Ok(InboxCredentials {
            address: self.address.clone(),
            recovery_key: self.receiver.identity.export().to_string(),
        })
    }

    /// Lists stored metadata (including previously consumed mail) without consuming it.
    /// Supports subject/sender filters, increasing-ID pagination and limits 1–100.
    pub async fn list_emails(&self, mut filters: Filters) -> Result<EmailPage> {
        self.require_open()?;
        filters.address = Some(self.address.clone());
        filters.validate()?;
        let limit = filters.limit.unwrap_or(20) as usize;
        let mut emails = Vec::new();

        loop {
            let candidates = self
                .receiver
                .storage
                .list(filters.clone(), Some(self.receiver.identity.fingerprint()))
                .await?;
            let exhausted = candidates.len() < limit;

            for email in candidates {
                filters.after_id = Some(email.id);
                if self.matches_inbox_filter(email.id).await? {
                    emails.push(email);
                    if emails.len() == limit {
                        break;
                    }
                }
            }

            if exhausted || emails.len() == limit {
                break;
            }
        }

        let next_cursor = if emails.len() == limit {
            emails.last().map(|email| email.id)
        } else {
            None
        };

        Ok(EmailPage {
            emails,
            next_cursor,
        })
    }

    /// Reads a stored message body and attachment metadata without consuming it.
    pub async fn get_email(&self, id: i64) -> Result<ParsedEmail> {
        self.check_email(id).await?;
        self.receiver.get_email(id, 256 * 1024).await
    }

    /// Reads a full BCL-compatible message from one stored MIME snapshot.
    pub async fn get_mail_message(&self, id: i64) -> Result<crate::mail::MailMessage> {
        self.check_email(id).await?;
        let (metadata, raw) = self.receiver.storage.email(id).await?;
        tokio::task::spawn_blocking(move || crate::mail::mail_message(metadata, &raw)).await?
    }

    /// Deletes one message from this inbox. Returns zero if it does not exist.
    pub async fn delete_email(&self, id: i64) -> Result<usize> {
        self.require_open()?;
        if id <= 0 {
            bail!("invalid_argument: email ID must be positive")
        }

        self.receiver
            .storage
            .delete_for_inbox(
                self.receiver.identity.fingerprint(),
                self.address.clone(),
                Some(id),
            )
            .await
    }

    /// Deletes all stored messages for this address, preserving identity and address.
    pub async fn clear_emails(&self) -> Result<usize> {
        self.require_open()?;
        self.receiver
            .storage
            .delete_for_inbox(
                self.receiver.identity.fingerprint(),
                self.address.clone(),
                None,
            )
            .await
    }

    /// Returns an attachment's safe metadata and decoded bytes. Never executes content.
    pub async fn attachment(
        &self,
        id: i64,
        attachment_id: String,
    ) -> Result<(Attachment, Vec<u8>)> {
        let email = self.get_email(id).await?;
        if !email
            .attachments
            .iter()
            .any(|value| value.attachment_id == attachment_id)
        {
            bail!("attachment_not_found: attachment does not exist")
        }

        self.receiver.attachment(id, attachment_id).await
    }

    /// Saves decoded bytes to an absolute new file or existing directory. Existing
    /// files are never overwritten; untrusted attachment filenames are sanitized.
    pub async fn save_attachment(
        &self,
        id: i64,
        attachment_id: String,
        destination: &Path,
    ) -> Result<PathBuf> {
        if !destination.is_absolute() {
            bail!("invalid_argument: attachment destination must be absolute")
        }

        let (metadata, bytes) = self.attachment(id, attachment_id).await?;
        let destination = destination.to_owned();
        tokio::task::spawn_blocking(move || {
            crate::mail::save_attachment(&metadata, &bytes, &destination)
        })
        .await?
    }

    /// Returns the original stored MIME message as bytes, without consuming it.
    pub async fn raw_email(&self, id: i64) -> Result<Vec<u8>> {
        self.check_email(id).await?;
        Ok(self.receiver.storage.email(id).await?.1)
    }

    /// Returns sanitized HTML. Remote resources are blocked unless explicitly enabled.
    pub async fn html_email(&self, id: i64, allow_remote: bool) -> Result<HtmlEmail> {
        self.check_email(id).await?;
        self.receiver.html_email(id, allow_remote).await
    }

    async fn check_email(&self, id: i64) -> Result<()> {
        self.require_open()?;
        if id <= 0 {
            bail!("invalid_argument: email ID must be positive")
        }

        let metadata = self
            .receiver
            .storage
            .list(
                Filters {
                    address: Some(self.address.clone()),
                    after_id: Some(id - 1),
                    limit: Some(1),
                    ..Default::default()
                },
                Some(self.receiver.identity.fingerprint()),
            )
            .await?;
        if metadata.first().is_none_or(|email| email.id != id) {
            bail!("email_not_found: email does not exist in this inbox")
        }
        if !self.matches_inbox_filter(id).await? {
            bail!("email_not_found: email does not match this inbox filter")
        }

        Ok(())
    }

    async fn matches_inbox_filter(&self, id: i64) -> Result<bool> {
        let Some(filter) = self.receiver.inbox_filter.get().cloned() else {
            return Ok(true);
        };
        let (metadata, raw) = self.receiver.storage.email(id).await?;
        tokio::task::spawn_blocking(move || {
            filter.matches(&raw, &metadata.recipient, &metadata.envelope_sender)
        })
        .await?
    }

    fn require_open(&self) -> Result<()> {
        if self.closed.is_cancelled() {
            bail!("inbox_closed: inbox is closed")
        }

        Ok(())
    }

    /// Disconnects and interrupts waiters. Temporary profiles are removed after
    /// in-flight operations release them; explicit persistent profiles are retained.
    pub async fn close(&self) {
        self.closed.cancel();
        self.receiver.stop().await;
    }
}

fn receive_deadline(timeout_ms: Option<u64>) -> Result<Option<tokio::time::Instant>> {
    match timeout_ms {
        None => Ok(None),
        Some(0) => bail!("invalid_argument: timeout_ms must be positive or omitted"),
        Some(value) => tokio::time::Instant::now()
            .checked_add(Duration::from_millis(value))
            .map(Some)
            .context("invalid_argument: timeout_ms is too large"),
    }
}

async fn wait_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(value) => tokio::time::sleep_until(value).await,
        None => std::future::pending().await,
    }
}

impl Drop for Inbox {
    fn drop(&mut self) {
        self.closed.cancel();
        self.receiver.request_stop();
    }
}
