#![forbid(unsafe_code)]
mod broker;
mod data;
mod limits;
mod metrics;
mod routing;
#[allow(dead_code)]
#[path = "../../core/src/secure.rs"]
mod secure;
mod smtp;
#[cfg(feature = "tunnel")]
mod tunnel;
// The envelope implementation is shared with the Rust client; no second wire format.
#[allow(dead_code)]
#[path = "../../core/src/protocol.rs"]
mod protocol;

use anyhow::{Context, Result, ensure};
use axum::{
    Extension, Json, Router,
    extract::{ConnectInfo, State, WebSocketUpgrade},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use broker::Broker;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject};
use serde_json::{Value, json};
use std::{
    env,
    io::Write,
    net::{IpAddr, SocketAddr},
    sync::{Arc, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinSet,
    time::timeout,
};
use tokio_rustls::{TlsAcceptor, TlsConnector};

/// Per-connection WebSocket read buffer. It fits every client frame, the largest
/// being an authentication or delivery reply of under 200 bytes, so it never grows.
pub const WEBSOCKET_READ_BUFFER: usize = 256;

pub fn event(name: &str, fields: Value) {
    let mut fields = fields.as_object().cloned().unwrap_or_default();
    fields.insert("msg".into(), json!(name));
    fields.insert("level".into(), json!("INFO"));
    fields.insert(
        "time_unix_ms".into(),
        json!(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ),
    );

    // A closed or failing log pipe must not panic a connection task.
    let _ = writeln!(std::io::stderr(), "{}", Value::Object(fields));
}

fn setting(key: &str, default: &str) -> String {
    env::var(key)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.into())
}

fn address(value: &str) -> Result<SocketAddr> {
    let value = if value.starts_with(':') {
        format!("0.0.0.0{value}")
    } else {
        value.into()
    };

    value.parse().context("invalid listener address")
}

/// Browser origins that may open client WebSockets, from AHRARA_WEB_ORIGINS.
static WEB_ORIGINS: OnceLock<Box<[String]>> = OnceLock::new();

/// Native clients send no Origin. Browsers always do, so only listed web apps
/// connect and other sites cannot use their visitors as relay clients.
pub fn origin_allowed(origin: Option<&HeaderValue>, allowed: &[String]) -> bool {
    origin.is_none_or(|origin| origin.is_empty() || allowed.iter().any(|item| origin == item))
}

pub fn web_origins() -> &'static [String] {
    WEB_ORIGINS.get().map_or(&[], |origins| origins)
}

/// Parses exact origins such as `https://app.ahrara.dev`: no path, slash or wildcard.
fn parse_web_origins(value: &str) -> Result<Box<[String]>> {
    value
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .map(|origin| {
            let uri: axum::http::Uri = origin
                .parse()
                .context("AHRARA_WEB_ORIGINS entries must be origins")?;
            ensure!(
                matches!(uri.scheme_str(), Some("https" | "http"))
                    && uri.authority().is_some_and(|authority| {
                        !authority.as_str().contains(['*', '@'])
                            && format!("{}://{authority}", uri.scheme_str().unwrap_or_default())
                                == origin
                    }),
                "AHRARA_WEB_ORIGINS entries must be exact origins such as https://app.ahrara.dev"
            );
            Ok(origin.to_owned())
        })
        .collect()
}

/// The client address Cloudflare forwards. Trust it only where only Cloudflare
/// connects: the integrated Tunnel and cloudflared's private Unix socket.
pub fn client_ip(headers: &HeaderMap) -> Option<IpAddr> {
    headers.get("cf-connecting-ip")?.to_str().ok()?.parse().ok()
}

async fn upgrade(
    State(broker): State<Arc<Broker>>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    // Only the TLS listener has a TCP peer; the Unix socket is cloudflared's.
    let source = match peer {
        Some(Extension(ConnectInfo(peer))) => Some(peer.ip()),
        None => client_ip(&headers),
    };
    if !origin_allowed(headers.get("origin"), web_origins()) {
        metrics::rule("origin_not_allowed", source);
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    let Some(client) = broker.admission.try_client(source) else {
        return (StatusCode::TOO_MANY_REQUESTS, "too many connections").into_response();
    };

    ws.read_buffer_size(WEBSOCKET_READ_BUFFER)
        .write_buffer_size(0)
        .max_write_buffer_size(protocol::MAX_FRAME * 2)
        .max_message_size(protocol::MAX_CONTROL + 6 + secure::TAG_LEN)
        .max_frame_size(protocol::MAX_CONTROL + 6 + secure::TAG_LEN)
        .on_upgrade(move |socket| broker.serve(socket, client))
}

fn tls_config() -> Result<Arc<rustls::ServerConfig>> {
    let certs = CertificateDer::pem_file_iter(setting("AHRARA_TLS_CERT", "/certs/server.pem"))?
        .collect::<Result<Vec<_>, _>>()?;
    let key = PrivateKeyDer::from_pem_file(setting("AHRARA_TLS_KEY", "/certs/server-key.pem"))
        .context("missing TLS private key")?;

    Ok(Arc::new(
        rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)?,
    ))
}

async fn healthcheck() -> Result<()> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in CertificateDer::pem_file_iter(setting("AHRARA_TLS_CA", "/certs/ca.pem"))? {
        roots.add(cert?)?;
    }

    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let port = address(&setting("AHRARA_HTTPS_BIND", ":8443"))?.port();
    let name = setting("AHRARA_HEALTHCHECK_SERVER_NAME", "localhost");
    let server_name = ServerName::try_from(name.clone())?;
    timeout(Duration::from_secs(3), async {
        let tcp = TcpStream::connect(("127.0.0.1", port)).await?;
        let mut tls = TlsConnector::from(Arc::new(config))
            .connect(server_name, tcp)
            .await?;
        tls.write_all(
            format!("GET /healthz HTTP/1.1\r\nHost: {name}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await?;
        let mut response = String::new();
        tokio::io::BufReader::new(tls)
            .read_line(&mut response)
            .await?;
        ensure!(response.starts_with("HTTP/1.1 200 "), "unhealthy");
        Ok::<_, anyhow::Error>(())
    })
    .await??;

    Ok(())
}

/// Replies and closes without creating a TLS task or waiting for a slot. Tokio's
/// `try_write` skips a socket the reactor has not yet seen writable, so write the
/// fresh socket's empty send buffer directly.
fn refuse(tcp: TcpStream, reply: &[u8]) {
    if let Ok(mut tcp) = tcp.into_std() {
        let _ = tcp.write_all(reply);
    }
}

async fn smtp_listener(
    listener: TcpListener,
    broker: Arc<Broker>,
    tls: TlsAcceptor,
    forwarding: Arc<routing::Config>,
) -> Result<()> {
    let mut sessions = JoinSet::new();
    loop {
        tokio::select! {
            _ = broker.stopped.cancelled() => break,
            _ = sessions.join_next(), if !sessions.is_empty() => {},
            accepted = listener.accept() => {
                let (tcp, address) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        // Descriptor exhaustion is transient and must not stop the relay.
                        event("smtp_accept_failed", json!({"error": error.to_string()}));
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                if forwarding.refuses(address.ip()) {
                    metrics::rule("smtp_routing_only", Some(address.ip()));
                    refuse(tcp, b"554 5.7.1 Delivery only through Email Routing\r\n");
                    continue;
                }
                let trusted = forwarding.trusted(address.ip());
                let Some(permit) = broker.admission.try_source(address.ip(), trusted) else {
                    refuse(tcp, b"421 4.3.2 Server busy, retry later\r\n");
                    continue;
                };
                let broker = broker.clone();
                let tls = tls.clone();
                let forwarding = forwarding.clone();

                sessions.spawn(async move {
                    let _permit = permit;
                    smtp::serve(tcp, broker, tls, forwarding).await;
                });
            }
        }
    }

    let _ = timeout(Duration::from_secs(5), async {
        while sessions.join_next().await.is_some() {}
    })
    .await;

    Ok(())
}

/// The public routes, served on the TLS listener and cloudflared's Unix socket.
fn router(broker: Arc<Broker>) -> Router {
    Router::new()
        .route("/", get(upgrade))
        .route("/healthz", get(|| async { Json(json!({"status": "ok"})) }))
        .with_state(broker)
}

async fn stop_signal() -> Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            _ = terminate.recv() => {},
            result = tokio::signal::ctrl_c() => result?
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    Ok(())
}

#[cfg(unix)]
struct UnixSocket {
    path: std::path::PathBuf,
    device: u64,
    inode: u64,
}

#[cfg(unix)]
impl Drop for UnixSocket {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;
        if std::fs::symlink_metadata(&self.path)
            .is_ok_and(|metadata| metadata.dev() == self.device && metadata.ino() == self.inode)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(unix)]
fn bind_unix(path: &str) -> Result<(tokio::net::UnixListener, UnixSocket)> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    // Never unlink arbitrary files or a live socket. A socket nobody accepts on is
    // left behind by a crash or SIGKILL, so replace it instead of failing forever.
    let listener = match tokio::net::UnixListener::bind(path) {
        Err(error)
            if error.kind() == std::io::ErrorKind::AddrInUse
                && std::fs::symlink_metadata(path)?.file_type().is_socket()
                && std::os::unix::net::UnixStream::connect(path)
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::ConnectionRefused) =>
        {
            std::fs::remove_file(path)?;
            tokio::net::UnixListener::bind(path)?
        }
        result => result.with_context(|| format!("cannot bind Unix socket {path}"))?,
    };
    let metadata = std::fs::symlink_metadata(path)?;
    let socket = UnixSocket {
        path: path.into(),
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok((listener, socket))
}

async fn run() -> Result<()> {
    let command = env::args().nth(1);
    if matches!(
        command.as_deref(),
        Some("transport-keygen" | "transport-public-key")
    ) {
        let path = env::args()
            .nth(2)
            .context("usage: ahrara-server transport-keygen|transport-public-key <path>")?;
        ensure!(
            env::args().nth(3).is_none(),
            "usage: ahrara-server transport-keygen|transport-public-key <path>"
        );
        let path = std::path::Path::new(&path);
        println!(
            "{}",
            if command.as_deref() == Some("transport-keygen") {
                secure::keygen(path)?
            } else {
                secure::public_key(path)?
            }
        );

        return Ok(());
    }

    let _ = rustls::crypto::ring::default_provider().install_default();
    if env::args().nth(1).as_deref() == Some("healthcheck") {
        return healthcheck().await;
    }
    let _ = WEB_ORIGINS.set(parse_web_origins(&setting("AHRARA_WEB_ORIGINS", ""))?);
    let forwarding = Arc::new(routing::Config::new(
        &setting("AHRARA_TRUSTED_EMAIL_ROUTING_CIDRS", ""),
        &setting("AHRARA_EMAIL_ROUTING_ADDRESS", ""),
        &setting("AHRARA_EMAIL_ROUTING_VERIFICATION_RECIPIENT", ""),
    )?);

    #[cfg(feature = "tunnel")]
    let tunnel_configuration = tunnel::configuration()?;

    let smtp_address = address(&setting("AHRARA_SMTP_BIND", ":2525"))?;
    let https_address = address(&setting("AHRARA_HTTPS_BIND", ":8443"))?;
    // Off unless configured; keep it on a private network, never published.
    let metrics_address = env::var("AHRARA_METRICS_BIND")
        .ok()
        .filter(|value| !value.is_empty())
        .map(|value| address(&value))
        .transpose()?;
    let tls = tls_config()?;

    let smtp_socket = TcpListener::bind(smtp_address).await?;
    let transport_key = secure::load_key(std::path::Path::new(&setting(
        "AHRARA_TRANSPORT_KEY_FILE",
        "/certs/transport.key",
    )))?;
    let limits = limits::RuntimeLimits::from_env()?;
    let broker = Broker::encrypted_with_limits(*transport_key, limits);
    drop(transport_key);
    let sweep = tokio::spawn(broker.clone().sweep());
    let router = router(broker.clone());
    #[cfg(unix)]
    let unix_listener = env::var("AHRARA_UNIX_SOCKET")
        .ok()
        .filter(|path| !path.is_empty())
        .map(|path| bind_unix(&path))
        .transpose()?;
    #[cfg(unix)]
    let (unix_listener, _unix_socket) = match unix_listener {
        Some((listener, socket)) => (Some(listener), Some(socket)),
        None => (None, None),
    };
    let handle = axum_server::Handle::new();
    let mut https = axum_server::bind_rustls(
        https_address,
        axum_server::tls_rustls::RustlsConfig::from_config(tls.clone()),
    )
    .map(|acceptor| acceptor.acceptor(axum_server::accept::NoDelayAcceptor::new()))
    .handle(handle.clone());
    https
        .http_builder()
        .http1()
        .max_buf_size(16 * 1024)
        .timer(hyper_util::rt::TokioTimer::new())
        .header_read_timeout(Duration::from_secs(10));

    let mut web = JoinSet::new();
    let https_router = router.clone();
    web.spawn(async move {
        https
            .serve(https_router.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .map_err(anyhow::Error::from)
    });
    if let Some(metrics_address) = metrics_address {
        let listener = TcpListener::bind(metrics_address).await?;
        let router = metrics::router(broker.clone());
        let stopped = broker.stopped.clone();
        web.spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(stopped.cancelled_owned())
                .await
                .map_err(anyhow::Error::from)
        });
    }
    #[cfg(feature = "tunnel")]
    if let Some(configuration) = tunnel_configuration {
        web.spawn(tunnel::serve(configuration, broker.clone()));
    }
    #[cfg(unix)]
    if let Some(listener) = unix_listener {
        let stopped = broker.stopped.clone();
        web.spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(stopped.cancelled_owned())
                .await
                .map_err(anyhow::Error::from)
        });
    }

    let mut mail = tokio::spawn(smtp_listener(
        smtp_socket,
        broker.clone(),
        TlsAcceptor::from(tls),
        forwarding,
    ));
    event(
        "server_started",
        json!({"smtp": smtp_address, "https": https_address, "metrics": metrics_address}),
    );
    let result = tokio::select! {result=web.join_next()=>result.context("web listener stopped")?.map_err(anyhow::Error::from)?,result=&mut mail=>result?,result=stop_signal()=>result};
    broker.stopped.cancel();
    handle.graceful_shutdown(Some(Duration::from_secs(5)));
    if !mail.is_finished() {
        let _ = timeout(Duration::from_secs(5), &mut mail).await;
    }

    let _ = timeout(Duration::from_secs(5), async {
        while web.join_next().await.is_some() {}
    })
    .await;
    sweep.abort();
    result
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!(
            "{}",
            json!({"level":"ERROR","msg":"server_failed","error":error.to_string()})
        );

        std::process::exit(1);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    async fn connect(address: SocketAddr, source: &str) -> TcpStream {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind(format!("{source}:0").parse().unwrap()).unwrap();
        socket.connect(address).await.unwrap()
    }

    async fn greeting(stream: &mut TcpStream) -> String {
        let mut line = String::new();
        timeout(
            Duration::from_secs(2),
            tokio::io::BufReader::new(stream).read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();

        line
    }

    fn tls() -> TlsAcceptor {
        let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(rustls::server::ResolvesServerCertUsingSni::new()));
        TlsAcceptor::from(Arc::new(tls))
    }

    #[test]
    fn only_exact_listed_web_origins_may_open_client_websockets() {
        let allowed = parse_web_origins(" https://app.ahrara.dev, http://localhost:8788 ").unwrap();

        assert_eq!(
            &*allowed,
            ["https://app.ahrara.dev", "http://localhost:8788"]
        );
        for invalid in [
            "https://app.ahrara.dev/",
            "https://app.ahrara.dev/inbox",
            "https://*.ahrara.dev",
            "https://user@app.ahrara.dev",
            "app.ahrara.dev",
            "ftp://app.ahrara.dev",
        ] {
            assert!(parse_web_origins(invalid).is_err(), "{invalid}");
        }

        let origin = |value: &'static str| HeaderValue::from_static(value);

        assert!(origin_allowed(None, &[]), "native clients send no Origin");
        assert!(origin_allowed(Some(&origin("")), &[]));
        assert!(origin_allowed(
            Some(&origin("https://app.ahrara.dev")),
            &allowed
        ));
        assert!(!origin_allowed(
            Some(&origin("https://app.ahrara.dev")),
            &[]
        ));
        assert!(!origin_allowed(
            Some(&origin("https://evil.test")),
            &allowed
        ));
        assert!(!origin_allowed(
            Some(&origin("https://app.ahrara.dev.evil.test")),
            &allowed
        ));
    }

    #[tokio::test]
    async fn email_routing_sink_refuses_direct_smtp_from_untrusted_sources() {
        let broker = Broker::encrypted([1; 32]);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let forwarding = routing::Config::new("127.0.0.2/32", "relay@ingest.example.test", "");
        let task = tokio::spawn(smtp_listener(
            listener,
            broker.clone(),
            tls(),
            Arc::new(forwarding.unwrap()),
        ));
        let mut provider = connect(address, "127.0.0.2").await;

        assert!(greeting(&mut provider).await.starts_with("220 "));
        let mut direct = connect(address, "127.0.0.3").await;

        assert!(greeting(&mut direct).await.starts_with("554 "));
        broker.stopped.cancel();
        timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn smtp_admission_rejects_excess_sessions_and_idle_timeout_releases_slots() {
        let broker = Broker::encrypted_with_limits(
            [1; 32],
            limits::RuntimeLimits {
                smtp_sessions: 2,
                smtp_per_source: 1,
                command_timeout: Duration::from_secs(1),
                ..limits::RuntimeLimits::default()
            },
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(smtp_listener(
            listener,
            broker.clone(),
            tls(),
            Arc::new(routing::Config::new("", "", "").unwrap()),
        ));
        let mut first = connect(address, "127.0.0.2").await;

        assert!(greeting(&mut first).await.starts_with("220 "));
        let mut same_source = connect(address, "127.0.0.2").await;

        assert!(greeting(&mut same_source).await.starts_with("421 "));
        let mut other = connect(address, "127.0.0.3").await;

        assert!(greeting(&mut other).await.starts_with("220 "));
        let mut full = connect(address, "127.0.0.4").await;

        assert!(greeting(&mut full).await.starts_with("421 "));
        assert!(
            greeting(&mut first).await.is_empty(),
            "idle session did not expire"
        );
        assert!(greeting(&mut other).await.is_empty());
        let mut recovered = connect(address, "127.0.0.2").await;

        assert!(greeting(&mut recovered).await.starts_with("220 "));
        drop(recovered);
        broker.stopped.cancel();
        timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    async fn status(address: SocketAddr, path: &str) -> String {
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: relay\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        greeting(&mut stream).await
    }

    #[tokio::test]
    async fn metrics_are_served_only_by_their_own_listener() {
        let broker = Broker::encrypted([1; 32]);
        let public = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let private = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (public_address, private_address) =
            (public.local_addr().unwrap(), private.local_addr().unwrap());
        tokio::spawn(axum::serve(public, router(broker.clone())).into_future());
        tokio::spawn(axum::serve(private, metrics::router(broker)).into_future());

        assert!(
            status(public_address, "/metrics")
                .await
                .starts_with("HTTP/1.1 404 ")
        );
        assert!(
            status(public_address, "/healthz")
                .await
                .starts_with("HTTP/1.1 200 ")
        );
        assert!(
            status(private_address, "/metrics")
                .await
                .starts_with("HTTP/1.1 200 ")
        );
        assert!(
            status(private_address, "/healthz")
                .await
                .starts_with("HTTP/1.1 404 ")
        );
    }

    #[tokio::test]
    async fn unix_listener_refuses_existing_files_and_cleans_up_only_its_socket() {
        let path = std::env::temp_dir().join(format!(
            "ahrara-socket-test-{}-{}.sock",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::write(&path, b"keep").unwrap();

        assert!(bind_unix(path.to_str().unwrap()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"keep");
        std::fs::remove_file(&path).unwrap();
        let (listener, socket) = bind_unix(path.to_str().unwrap()).unwrap();

        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let client = tokio::net::UnixStream::connect(&path).await.unwrap();
        let _accepted = listener.accept().await.unwrap();
        drop(client);
        drop(listener);
        drop(socket);

        assert!(!path.exists());
        let (listener, socket) = bind_unix(path.to_str().unwrap()).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        drop(listener);
        drop(socket);

        assert_eq!(std::fs::read(&path).unwrap(), b"replacement");
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn unix_listener_replaces_only_stale_sockets() {
        let path = std::env::temp_dir().join(format!(
            "ahrara-stale-socket-test-{}-{}.sock",
            std::process::id(),
            rand::random::<u64>()
        ));
        let live = std::os::unix::net::UnixListener::bind(&path).unwrap();

        assert!(bind_unix(path.to_str().unwrap()).is_err());
        assert!(std::os::unix::net::UnixStream::connect(&path).is_ok());
        // Like a crashed relay: the socket file outlives its listener.
        drop(live);

        let (listener, socket) = bind_unix(path.to_str().unwrap()).unwrap();
        let client = tokio::net::UnixStream::connect(&path).await.unwrap();
        let _accepted = listener.accept().await.unwrap();
        drop(client);
        drop(listener);
        drop(socket);

        assert!(!path.exists());
    }
}
