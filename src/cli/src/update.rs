//! Signed, opt-in binary updates. This module never opens a profile or SQLite.
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
use reqwest::{Client, Url};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::AsyncWriteExt;

// The signature, not the host, makes the manifest trustworthy; updates only move forward.
const MANIFEST_URL: &str = "https://github.com/Hitmasu/Ahrara/releases/latest/download/stable.json";
const SIGNING_CONTEXT: &[u8] = b"ahrara-release-v1\0";
const MANIFEST_LIMIT: u64 = 128 * 1024;
const BINARY_LIMIT: u64 = 256 * 1024 * 1024;
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum InstallMethod {
    Sh,
    Powershell,
    Manual,
    Brew,
    Npm,
    Pip,
    Plugin,
    #[value(skip)]
    Unknown,
}

impl InstallMethod {
    fn direct(self) -> bool {
        matches!(self, Self::Sh | Self::Powershell | Self::Manual)
    }

    fn channel(self) -> &'static str {
        match self {
            Self::Brew => "brew",
            Self::Npm => "npm",
            Self::Pip => "pip",
            _ => "direct",
        }
    }

    fn instruction(self) -> &'static str {
        match self {
            Self::Brew => "brew upgrade ahrara",
            Self::Npm => "npm install -g ahrara@latest",
            Self::Pip => {
                "python -m pip install --upgrade ahrara (in the installation's Python environment)"
            }
            Self::Plugin => "Update through the Ahrara plugin: node src/plugin/ahrara.mjs --update",
            Self::Unknown => {
                "Download a binary from https://github.com/Hitmasu/Ahrara/releases, or opt in with: ahrara update --register manual"
            }
            _ => "ahrara update",
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema_version: u32,
    method: InstallMethod,
}

fn sidecar(executable: &Path, suffix: &str) -> PathBuf {
    let mut path = executable.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

pub fn install_method(executable: &Path) -> Result<InstallMethod> {
    let file = match File::open(sidecar(executable, ".install.json")) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(InstallMethod::Unknown),
        Err(e) => return Err(e.into()),
    };
    let receipt: Receipt = serde_json::from_reader(file.take(4096)).context(
        "invalid installation receipt; reinstall using your original installation method",
    )?;
    ensure!(
        receipt.schema_version == 1,
        "unsupported installation receipt"
    );

    Ok(receipt.method)
}

/// Package installers can also ship this small sidecar without executing the binary.
/// Registering is explicit and never changes an existing installation's origin.
pub fn register(executable: &Path, method: InstallMethod) -> Result<()> {
    ensure!(
        method != InstallMethod::Unknown,
        "choose an installation method"
    );
    let _lock = installation_lock(executable, true)?;
    let existing = install_method(executable)?;
    ensure!(
        existing == InstallMethod::Unknown || existing == method,
        "installation already registered as {existing:?}; use the original installer"
    );
    if existing == method {
        return Ok(());
    }

    let mut temp = tempfile::NamedTempFile::new_in(
        executable
            .parent()
            .context("executable directory unavailable")?,
    )?;
    serde_json::to_writer(
        &mut temp,
        &Receipt {
            schema_version: 1,
            method,
        },
    )?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(sidecar(executable, ".install.json"))
        .map_err(|e| e.error)?;
    Ok(())
}

fn installation_lock(executable: &Path, exclusive: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(sidecar(executable, ".update.lock"))
        .context("cannot open installation lock; the installation directory must be writable")?;
    let result = if exclusive {
        file.try_lock()
    } else {
        file.try_lock_shared()
    };
    result.context(
        "installation busy: stop other Ahrara processes using this executable before updating",
    )?;

    Ok(file)
}

/// Held for the lifetime of every command in a managed direct installation,
/// across all profiles. Package-owned and unregistered binaries are never replaced.
pub fn running_guard() -> Result<Option<File>> {
    let exe = std::env::current_exe()?.canonicalize()?;
    if install_method(&exe)?.direct() {
        return Ok(Some(installation_lock(&exe, false)?));
    }

    Ok(None)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedManifest {
    /// Base64 of the exact UTF-8 JSON bytes; no cross-language JSON canonicalization.
    payload: String,
    signature: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    channels: BTreeMap<String, Release>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Release {
    version: Version,
    artifacts: BTreeMap<String, Artifact>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    url: String,
    sha256: String,
    size: u64,
}

fn verify_manifest(bytes: &[u8], key: &VerifyingKey) -> Result<Manifest> {
    ensure!(
        bytes.len() as u64 <= MANIFEST_LIMIT,
        "release manifest is too large"
    );
    let signed: SignedManifest =
        serde_json::from_slice(bytes).context("invalid release manifest")?;
    let payload = STANDARD
        .decode(&signed.payload)
        .context("invalid manifest encoding")?;
    let signature = Signature::from_slice(&STANDARD.decode(&signed.signature)?)?;
    let mut message = SIGNING_CONTEXT.to_vec();
    message.extend_from_slice(&payload);
    key.verify_strict(&message, &signature)
        .context("release signature verification failed")?;
    let manifest: Manifest =
        serde_json::from_slice(&payload).context("invalid signed release metadata")?;
    ensure!(
        manifest.schema_version == 1,
        "unsupported release manifest version"
    );

    for (channel, release) in &manifest.channels {
        ensure!(
            ["direct", "brew", "npm", "pip"].contains(&channel.as_str()),
            "unknown release channel"
        );
        ensure!(
            release.version.pre.is_empty() && release.version.build.is_empty(),
            "stable releases must use a stable version"
        );

        for artifact in release.artifacts.values() {
            https_url(&artifact.url)?;
            ensure!(
                artifact.size > 0 && artifact.size <= BINARY_LIMIT,
                "invalid release binary size"
            );

            ensure!(
                hex::decode(&artifact.sha256).is_ok_and(|hash| hash.len() == 32),
                "invalid release checksum"
            );
        }
    }

    Ok(manifest)
}

fn https_url(value: &str) -> Result<Url> {
    let url = Url::parse(value)?;
    ensure!(
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "release URLs must use HTTPS without credentials or fragments"
    );

    Ok(url)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn platform() -> Result<String> {
    let os = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "darwin",
        "windows" => "windows",
        _ => bail!("this operating system has no release binary"),
    };
    let arch = std::env::consts::ARCH;
    ensure!(
        ["x86_64", "aarch64"].contains(&arch),
        "this architecture has no release binary"
    );

    Ok(format!("{os}-{arch}"))
}

struct Updater {
    client: Client,
    url: Url,
    key: VerifyingKey,
}

impl Updater {
    fn official() -> Result<Self> {
        // The trust root is embedded by the release build, never supplied by the
        // downloaded manifest, a profile, a receipt, or a runtime environment variable.
        let key = option_env!("AHRARA_UPDATE_PUBLIC_KEY")
            .filter(|key| !key.is_empty())
            .context("updates are not configured for this build: release builds require AHRARA_UPDATE_PUBLIC_KEY")?;
        let key: [u8; 32] = hex::decode(key)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid release public key"))?;

        Ok(Self {
            client: Client::builder()
                .https_only(true)
                .redirect(reqwest::redirect::Policy::limited(5))
                .connect_timeout(Duration::from_secs(5))
                // Binaries may be large on slow links: fail only stalled reads.
                // The manifest request keeps its own total timeout.
                .read_timeout(Duration::from_secs(30))
                .user_agent("Ahrara-Updater")
                .build()?,
            url: https_url(MANIFEST_URL)?,
            key: VerifyingKey::from_bytes(&key)?,
        })
    }

    async fn fetch_manifest(&self) -> Result<Vec<u8>> {
        let mut response = self
            .client
            .get(self.url.clone())
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .context("cannot reach the release service")?
            .error_for_status()?;
        ensure!(
            response
                .content_length()
                .is_none_or(|n| n <= MANIFEST_LIMIT),
            "release manifest is too large"
        );
        let mut bytes = Vec::new();

        while let Some(chunk) = response.chunk().await? {
            ensure!(
                bytes.len() as u64 + chunk.len() as u64 <= MANIFEST_LIMIT,
                "release manifest is too large"
            );

            bytes.extend_from_slice(&chunk);
        }

        Ok(bytes)
    }

    async fn download(
        &self,
        artifact: &Artifact,
        directory: &Path,
    ) -> Result<tempfile::NamedTempFile> {
        let mut response = self
            .client
            .get(https_url(&artifact.url)?)
            .send()
            .await?
            .error_for_status()?;
        ensure!(
            response.content_length().is_none_or(|n| n == artifact.size),
            "release binary size does not match manifest"
        );
        let temp = tempfile::NamedTempFile::new_in(directory)?;
        let mut output = tokio::fs::File::from_std(temp.reopen()?);
        let mut hash = Sha256::new();
        let mut size = 0_u64;

        while let Some(chunk) = response.chunk().await? {
            size += chunk.len() as u64;
            ensure!(
                size <= artifact.size && size <= BINARY_LIMIT,
                "release binary exceeds declared size"
            );
            hash.update(&chunk);
            output.write_all(&chunk).await?;
        }

        output.sync_all().await?;
        drop(output);
        ensure!(
            size == artifact.size,
            "release binary download is incomplete"
        );
        ensure!(
            hex::encode(hash.finalize()).eq_ignore_ascii_case(&artifact.sha256),
            "release checksum verification failed"
        );

        Ok(temp)
    }
}

#[derive(Debug, Serialize)]
pub struct UpdateStatus {
    pub installed_version: String,
    pub available_version: Option<String>,
    pub install_method: InstallMethod,
    pub platform: String,
    pub update_available: bool,
    pub updated: bool,
    pub message: String,
}

fn status(
    manifest: &Manifest,
    method: InstallMethod,
    installed: &Version,
    target: &str,
) -> UpdateStatus {
    let release = manifest.channels.get(method.channel());
    // Missing artifacts mean this channel has not published for this platform yet.
    let release = release.filter(|r| r.artifacts.contains_key(target));
    let available = release.is_some_and(|r| r.version.cmp_precedence(installed).is_gt());
    UpdateStatus {
        installed_version: installed.to_string(),
        available_version: release.map(|r| r.version.to_string()),
        install_method: method,
        platform: target.into(),
        update_available: available,
        updated: false,
        message: if available {
            format!(
                "Ahrara {} is available. {}",
                release.unwrap().version,
                method.instruction()
            )
        } else if release.is_none() {
            "No release is available for this installation channel and platform yet.".into()
        } else {
            "Ahrara is up to date for this installation channel.".into()
        },
    }
}

pub async fn run(check: bool) -> Result<UpdateStatus> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let method = install_method(&exe)?;
    if !check && !method.direct() {
        return Ok(UpdateStatus {
            installed_version: env!("CARGO_PKG_VERSION").into(),
            available_version: None,
            install_method: method,
            platform: platform()?,
            update_available: false,
            updated: false,
            message: format!(
                "This executable will not be replaced. {}",
                method.instruction()
            ),
        });
    }

    let _guard = if check {
        None
    } else {
        Some(installation_lock(&exe, true)?)
    };
    let updater = Updater::official()?;
    let bytes = updater.fetch_manifest().await?;
    let manifest = verify_manifest(&bytes, &updater.key)?;
    let installed = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let mut result = status(&manifest, method, &installed, &platform()?);
    if check || !result.update_available {
        return Ok(result);
    }

    let artifact = &manifest.channels[method.channel()].artifacts[&result.platform];
    let temp = updater
        .download(
            artifact,
            exe.parent().context("executable directory unavailable")?,
        )
        .await?;
    replace_current(temp.path(), &exe)?;
    result.updated = true;
    result.message = format!(
        "Updated to Ahrara {}. Start Ahrara again when ready. Local profiles were not changed.",
        result.available_version.as_deref().unwrap()
    );

    Ok(result)
}

fn replace_current(download: &Path, executable: &Path) -> Result<()> {
    // Keep a recovery copy until replacement completes. In particular, Windows
    // needs to relocate the running image before putting the new one in its place.
    let mut backup = tempfile::NamedTempFile::new_in(
        executable
            .parent()
            .context("executable directory unavailable")?,
    )?;
    std::io::copy(&mut File::open(executable)?, &mut backup)?;
    let permissions = std::fs::metadata(executable)?.permissions();
    backup.as_file().set_permissions(permissions.clone())?;
    backup.as_file().sync_all()?;
    std::fs::set_permissions(download, permissions)?;
    if let Err(error) = self_replace::self_replace(download) {
        if !executable.exists()
            && let Err(recovery) = backup.persist_noclobber(executable)
        {
            let path = recovery.file.path().to_owned();
            // The command exits on this error. Keep the recovery file on disk
            // even if the filesystem cannot rename it; the OS closes its handle.
            std::mem::forget(recovery.file);
            bail!(
                "replacement failed ({error}); restore the executable from {}",
                path.display()
            );
        }

        return Err(error).context("binary replacement failed; the previous binary was preserved");
    }

    Ok(())
}

#[derive(Serialize, Deserialize)]
struct CheckCache {
    checked_at: u64,
    manifest: Option<String>,
}

fn cache_path(executable: &Path) -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("dev", "Ahrara", "ahrara")
        .context("cache directory unavailable")?;
    let digest = hex::encode(Sha256::digest(executable.as_os_str().as_encoded_bytes()));
    Ok(dirs.cache_dir().join(format!("update-{digest}.json")))
}

async fn cached_notice(
    updater: &Updater,
    method: InstallMethod,
    path: &Path,
) -> Result<Option<String>> {
    // Profiles sharing an executable also share the check budget. Never wait for
    // another process's network request, and never delete this lock's inode.
    crate::config::private_dir(path.parent().context("cache directory unavailable")?)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(sidecar(path, ".lock"))?;
    if lock.try_lock().is_err() {
        return Ok(None);
    }

    let time = now();
    let cached = File::open(path)
        .ok()
        .and_then(|f| serde_json::from_reader::<_, CheckCache>(f.take(MANIFEST_LIMIT * 2)).ok());
    let bytes = if let Some(cache) =
        cached.filter(|c| c.checked_at <= time && time - c.checked_at < CHECK_INTERVAL.as_secs())
    {
        cache.manifest.map(|m| STANDARD.decode(m)).transpose()?
    } else {
        let fetched = updater
            .fetch_manifest()
            .await
            .ok()
            .filter(|bytes| verify_manifest(bytes, &updater.key).is_ok());
        let cache = CheckCache {
            checked_at: time,
            manifest: fetched.as_ref().map(|b| STANDARD.encode(b)),
        };
        // Failed requests also get a TTL, so offline starts never hammer the endpoint.
        let _ = crate::config::private_write(path, &serde_json::to_vec(&cache)?, true);
        fetched
    };
    let Some(bytes) = bytes else { return Ok(None) };
    let manifest = verify_manifest(&bytes, &updater.key)?;
    let result = status(
        &manifest,
        method,
        &Version::parse(env!("CARGO_PKG_VERSION"))?,
        &platform()?,
    );

    Ok(result.update_available.then_some(result.message))
}

/// Only the interactive listener calls this; JSON/MCP/API output stays unchanged.
/// Errors are deliberately silent here. Explicit `update --check` reports them.
pub async fn notifications(sender: tokio::sync::mpsc::Sender<String>) {
    let Ok(updater) = Updater::official() else {
        return;
    };
    let Ok(exe) = std::env::current_exe().and_then(|p| p.canonicalize()) else {
        return;
    };
    let Ok(method) = install_method(&exe) else {
        return;
    };
    let Ok(path) = cache_path(&exe) else { return };
    let mut last = None;

    loop {
        if let Ok(Some(message)) = cached_notice(&updater, method, &path).await
            && last.as_ref() != Some(&message)
        {
            last = Some(message.clone());
            if sender.send(message).await.is_err() {
                return;
            }
        }

        tokio::select! {
            _ = sender.closed() => return,
            _ = tokio::time::sleep(CHECK_INTERVAL) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn sign(manifest: &Manifest, key: &SigningKey) -> Vec<u8> {
        let payload = serde_json::to_vec(manifest).unwrap();
        let mut message = SIGNING_CONTEXT.to_vec();
        message.extend_from_slice(&payload);
        serde_json::to_vec(&SignedManifest {
            payload: STANDARD.encode(payload),
            signature: STANDARD.encode(key.sign(&message).to_bytes()),
        })
        .unwrap()
    }

    fn release(version: &str, url: &str, bytes: &[u8]) -> Release {
        Release {
            version: Version::parse(version).unwrap(),
            artifacts: BTreeMap::from([(
                platform().unwrap(),
                Artifact {
                    url: url.into(),
                    sha256: hex::encode(Sha256::digest(bytes)),
                    size: bytes.len() as u64,
                },
            )]),
        }
    }

    fn manifest(url: &str, bytes: &[u8]) -> Manifest {
        Manifest {
            schema_version: 1,
            channels: BTreeMap::from([("direct".into(), release("0.10.0", url, bytes))]),
        }
    }

    #[test]
    fn signatures_schema_urls_and_versions_are_checked() {
        let key = SigningKey::generate(&mut rand::rngs::OsRng);
        let mut m = manifest("https://example.test/binary", b"binary");
        let bytes = sign(&m, &key);

        assert!(verify_manifest(&bytes, &key.verifying_key()).is_ok());
        let wrong = SigningKey::generate(&mut rand::rngs::OsRng);

        assert!(verify_manifest(&bytes, &wrong.verifying_key()).is_err());
        let mut tampered: SignedManifest = serde_json::from_slice(&bytes).unwrap();
        tampered.payload = STANDARD.encode(b"changed");

        assert!(
            verify_manifest(
                &serde_json::to_vec(&tampered).unwrap(),
                &key.verifying_key()
            )
            .is_err()
        );
        m.schema_version = 2;

        assert!(verify_manifest(&sign(&m, &key), &key.verifying_key()).is_err());
        m.schema_version = 1;

        for url in [
            "http://example.test/bin",
            "https://user:pass@example.test/bin",
            "file:///bin",
            "https://example.test/bin#fragment",
        ] {
            m.channels
                .insert("direct".into(), release("0.10.0", url, b"binary"));
            assert!(verify_manifest(&sign(&m, &key), &key.verifying_key()).is_err());
        }

        m.channels.insert(
            "direct".into(),
            release("0.10.0-beta.1", "https://example.test/bin", b"binary"),
        );

        assert!(verify_manifest(&sign(&m, &key), &key.verifying_key()).is_err());
        assert!(
            verify_manifest(&vec![0; MANIFEST_LIMIT as usize + 1], &key.verifying_key()).is_err()
        );
    }

    #[test]
    fn installation_channel_controls_available_version_and_no_downgrades() {
        let mut m = manifest("https://example.test/binary", b"binary");
        m.channels.insert(
            "brew".into(),
            release("0.1.0", "https://example.test/old", b"old"),
        );
        let installed = Version::parse("0.2.0").unwrap();

        assert!(
            status(&m, InstallMethod::Manual, &installed, &platform().unwrap()).update_available
        );
        assert!(
            !status(&m, InstallMethod::Brew, &installed, &platform().unwrap()).update_available
        );
        assert!(!status(&m, InstallMethod::Npm, &installed, &platform().unwrap()).update_available);
        assert!(
            !status(
                &m,
                InstallMethod::Manual,
                &installed,
                "unpublished-platform"
            )
            .update_available
        );
        assert!(
            !status(
                &m,
                InstallMethod::Manual,
                &Version::parse("0.10.0").unwrap(),
                &platform().unwrap()
            )
            .update_available
        );

        for method in [
            InstallMethod::Brew,
            InstallMethod::Npm,
            InstallMethod::Pip,
            InstallMethod::Plugin,
        ] {
            assert!(!method.direct());
            assert!(!method.instruction().starts_with("ahrara update"));
        }
    }

    #[test]
    fn receipts_do_not_guess_or_change_package_ownership() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("ahrara");

        assert_eq!(install_method(&exe).unwrap(), InstallMethod::Unknown);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        register(&exe, InstallMethod::Brew).unwrap();
        register(&exe, InstallMethod::Brew).unwrap();

        assert!(register(&exe, InstallMethod::Sh).is_err());
        assert_eq!(install_method(&exe).unwrap(), InstallMethod::Brew);
        std::fs::write(sidecar(&exe, ".install.json"), "broken").unwrap();

        assert!(install_method(&exe).is_err());
    }

    #[test]
    fn listener_and_update_locks_conflict_and_release() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("ahrara");
        let first = installation_lock(&exe, false).unwrap();
        let second = installation_lock(&exe, false).unwrap();

        assert!(installation_lock(&exe, true).is_err());
        drop(first);
        drop(second);
        let update = installation_lock(&exe, true).unwrap();

        assert!(installation_lock(&exe, false).is_err());
        assert!(installation_lock(&exe, true).is_err());
        drop(update);

        assert!(installation_lock(&exe, false).is_ok());
    }

    struct Server {
        updater: Updater,
        manifest: Manifest,
        requests: Arc<AtomicUsize>,
        handle: axum_server::Handle<std::net::SocketAddr>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.handle.shutdown();
            self.task.abort();
        }
    }

    async fn server() -> Server {
        use axum::{
            Router,
            body::Body,
            response::{IntoResponse, Response},
            routing::get,
        };
        let _ = rustls::crypto::ring::default_provider().install_default();
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
            .unwrap();
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem(
            cert.cert.pem().into_bytes(),
            cert.signing_key.serialize_pem().into_bytes(),
        )
        .await
        .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("https://{}", listener.local_addr().unwrap());
        let key = SigningKey::generate(&mut rand::rngs::OsRng);
        let m = manifest(&format!("{base}/binary"), b"verified-binary");
        let signed = sign(&m, &key);
        let count = Arc::new(AtomicUsize::new(0));
        let requests = count.clone();
        let app = Router::new()
            .route(
                "/stable.json",
                get(move || {
                    count.fetch_add(1, Ordering::SeqCst);
                    let b = signed.clone();
                    async { b }
                }),
            )
            .route("/binary", get(|| async { "verified-binary" }))
            .route(
                "/large",
                get(|| async { vec![0_u8; MANIFEST_LIMIT as usize + 1] }),
            )
            .route(
                "/slow",
                get(|| async {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    "late"
                }),
            )
            .route(
                "/stream",
                get(|| async {
                    let chunks = futures_util::stream::iter([
                        Ok::<_, std::io::Error>(vec![0_u8; 10]),
                        Ok(vec![0_u8; 100]),
                    ]);

                    Response::new(Body::from_stream(chunks))
                }),
            )
            .route(
                "/redirect",
                get(|| async {
                    (
                        axum::http::StatusCode::FOUND,
                        [("location", "http://127.0.0.1/unsafe")],
                    )
                        .into_response()
                }),
            );
        let handle = axum_server::Handle::new();
        let h = handle.clone();
        let task = tokio::spawn(async move {
            axum_server::from_tcp_rustls(listener, tls)
                .unwrap()
                .handle(h)
                .serve(app.into_make_service())
                .await
                .unwrap();
        });
        let client = Client::builder()
            .https_only(true)
            .no_proxy()
            .timeout(Duration::from_millis(500))
            .add_root_certificate(
                reqwest::Certificate::from_pem(cert.cert.pem().as_bytes()).unwrap(),
            )
            .build()
            .unwrap();

        Server {
            updater: Updater {
                client,
                url: Url::parse(&format!("{base}/stable.json")).unwrap(),
                key: key.verifying_key(),
            },
            manifest: m,
            requests,
            handle,
            task,
        }
    }

    #[tokio::test]
    async fn https_manifest_download_integrity_limits_and_tls_failure() {
        let mut s = server().await;
        let bytes = s.updater.fetch_manifest().await.unwrap();
        verify_manifest(&bytes, &s.updater.key).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let artifact = &s.manifest.channels["direct"].artifacts[&platform().unwrap()];
        let downloaded = s.updater.download(artifact, dir.path()).await.unwrap();

        assert_eq!(
            std::fs::read(downloaded.path()).unwrap(),
            b"verified-binary"
        );
        drop(downloaded);

        for (hash, size) in [
            ("00".repeat(32), artifact.size),
            (artifact.sha256.clone(), 1),
            (artifact.sha256.clone(), artifact.size + 1),
        ] {
            let invalid = Artifact {
                sha256: hash,
                size,
                ..artifact.clone()
            };

            assert!(s.updater.download(&invalid, dir.path()).await.is_err());
        }

        let mut invalid = artifact.clone();
        invalid.url = s.updater.url.join("/stream").unwrap().to_string();

        assert!(s.updater.download(&invalid, dir.path()).await.is_err());
        invalid.url = s.updater.url.join("/slow").unwrap().to_string();

        assert!(s.updater.download(&invalid, dir.path()).await.is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);

        for endpoint in ["/large", "/redirect", "/missing"] {
            s.updater.url.set_path(endpoint);
            assert!(s.updater.fetch_manifest().await.is_err());
        }

        s.updater.url.set_path("/stable.json");
        s.updater.client = Client::builder()
            .https_only(true)
            .no_proxy()
            .build()
            .unwrap();

        assert!(s.updater.fetch_manifest().await.is_err());
    }

    #[tokio::test]
    async fn notifications_cache_success_failure_and_verify_cached_signature() {
        let s = server().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        let (first, concurrent) = tokio::join!(
            cached_notice(&s.updater, InstallMethod::Manual, &path),
            cached_notice(&s.updater, InstallMethod::Manual, &path)
        );

        assert!(first.unwrap().is_some() || concurrent.unwrap().is_some());
        assert!(
            cached_notice(&s.updater, InstallMethod::Manual, &path)
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(s.requests.load(Ordering::SeqCst), 1);
        let mut cached: CheckCache =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        cached.manifest = Some(STANDARD.encode(b"tampered"));
        std::fs::write(&path, serde_json::to_vec(&cached).unwrap()).unwrap();

        assert!(
            cached_notice(&s.updater, InstallMethod::Manual, &path)
                .await
                .is_err()
        );
        cached.checked_at = now() - CHECK_INTERVAL.as_secs();
        std::fs::write(&path, serde_json::to_vec(&cached).unwrap()).unwrap();

        assert!(
            cached_notice(&s.updater, InstallMethod::Manual, &path)
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(s.requests.load(Ordering::SeqCst), 2);
        s.handle.shutdown();
        s.task.abort();
        let offline = dir.path().join("offline.json");

        assert!(
            cached_notice(&s.updater, InstallMethod::Manual, &offline)
                .await
                .unwrap()
                .is_none()
        );

        assert!(offline.exists());
    }

    // Real replacement in a child process: never mutate the test runner itself.
    #[test]
    fn replacement_child() {
        let Some(download) = std::env::var_os("AHRARA_TEST_REPLACEMENT") else {
            return;
        };
        let exe = std::env::current_exe().unwrap();
        let _guard = installation_lock(&exe, true).unwrap();
        replace_current(Path::new(&download), &exe).unwrap();
    }

    #[test]
    fn replacement_process_preserves_profiles_and_executable_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join(if cfg!(windows) {
            "ahrara.exe"
        } else {
            "ahrara"
        });
        std::fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();
        register(&exe, InstallMethod::Manual).unwrap();
        let profile = dir.path().join("profile");
        std::fs::create_dir(&profile).unwrap();

        for name in ["identity.key", "ahrara.db", "api.token", "config.toml"] {
            std::fs::write(profile.join(name), b"must-not-change").unwrap();
        }

        let download = dir.path().join("download");
        std::fs::copy(&exe, &download).unwrap();
        // A distinguishable, valid image (trailing bytes do not affect loading).
        {
            use std::io::Write;
            OpenOptions::new()
                .append(true)
                .open(&download)
                .unwrap()
                .write_all(b"replacement-marker")
                .unwrap();
        }
        let result = std::process::Command::new(&exe)
            .args(["--exact", "update::tests::replacement_child", "--nocapture"])
            .env("AHRARA_TEST_REPLACEMENT", &download)
            .output()
            .unwrap();

        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            std::fs::read(&exe).unwrap(),
            std::fs::read(&download).unwrap()
        );

        for name in ["identity.key", "ahrara.db", "api.token", "config.toml"] {
            assert_eq!(
                std::fs::read(profile.join(name)).unwrap(),
                b"must-not-change"
            );
        }

        assert_eq!(install_method(&exe).unwrap(), InstallMethod::Manual);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                std::fs::metadata(&exe).unwrap().permissions().mode() & 0o111,
                0
            );
        }
    }
}
