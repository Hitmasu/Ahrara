#![forbid(unsafe_code)]
mod auth;
mod broker;
mod data;
mod limits;
mod routing;
#[allow(dead_code)]
#[path = "../../core/src/secure.rs"]
mod secure;
mod smtp;
mod sse;
#[cfg(feature = "tunnel")]
mod tunnel;
// The envelope implementation is shared with the Rust client; no second wire format.
#[allow(dead_code)]
#[path = "../../core/src/protocol.rs"]
mod protocol;

use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use broker::Broker;
use rustls::pki_types::ServerName;
use serde_json::{Value, json};
use std::{
    env,
    fs::File,
    io::BufReader,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinSet,
    time::timeout,
};
use tokio_rustls::{TlsAcceptor, TlsConnector};

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

    eprintln!("{}", Value::Object(fields));
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

async fn upgrade(
    State(broker): State<Arc<Broker>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if headers.get("origin").is_some_and(|value| !value.is_empty()) {
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }

    ws.read_buffer_size(512)
        .write_buffer_size(0)
        .max_write_buffer_size(protocol::MAX_FRAME * 2)
        .max_message_size(protocol::MAX_CONTROL + 6 + secure::TAG_LEN)
        .max_frame_size(protocol::MAX_CONTROL + 6 + secure::TAG_LEN)
        .on_upgrade(move |socket| broker.serve(socket))
}

fn tls_config() -> Result<Arc<rustls::ServerConfig>> {
    let certs = rustls_pemfile::certs(&mut BufReader::new(File::open(setting(
        "AHRARA_TLS_CERT",
        "/certs/server.pem",
    ))?))
    .collect::<std::io::Result<Vec<_>>>()?;
    let key = rustls_pemfile::private_key(&mut BufReader::new(File::open(setting(
        "AHRARA_TLS_KEY",
        "/certs/server-key.pem",
    ))?))?
    .context("missing TLS private key")?;

    Ok(Arc::new(
        rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)?,
    ))
}

async fn healthcheck() -> Result<()> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut BufReader::new(File::open(setting(
        "AHRARA_TLS_CA",
        "/certs/ca.pem",
    ))?)) {
        roots.add(cert?)?;
    }

    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let port = address(&setting("AHRARA_HTTPS_BIND", ":8443"))?.port();
    timeout(Duration::from_secs(3), async {
        let tcp = TcpStream::connect(("127.0.0.1", port)).await?;
        let mut tls = TlsConnector::from(Arc::new(config))
            .connect(ServerName::try_from("localhost")?, tcp)
            .await?;
        tls.write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
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

async fn smtp_listener(
    listener: TcpListener,
    broker: Arc<Broker>,
    tls: TlsAcceptor,
    prefixes: Arc<Vec<ipnet::IpNet>>,
) -> Result<()> {
    let mut sessions = JoinSet::new();
    loop {
        tokio::select! {
            _ = broker.stopped.cancelled() => break,
            _ = sessions.join_next(), if !sessions.is_empty() => {},
            accepted = listener.accept() => {
                let (tcp, address) = accepted?;
                let trusted = routing::trusted(address.ip(), &prefixes);
                let Some(permit) = broker.admission.try_source(address.ip(), trusted) else {
                    // A rejected connection never creates a TLS task or waits for a slot.
                    let _ = tcp.try_write(b"421 4.3.2 Server busy, retry later\r\n");
                    continue;
                };
                let broker = broker.clone();
                let tls = tls.clone();
                let prefixes = prefixes.clone();

                sessions.spawn(async move {
                    let _permit = permit;
                    smtp::serve(tcp, broker, tls, prefixes).await;
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
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    // Refuse pre-existing paths, including stale sockets. Never unlink arbitrary files.
    let listener = tokio::net::UnixListener::bind(path)?;
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
    let prefixes = Arc::new(routing::prefixes(&setting(
        "AHRARA_TRUSTED_EMAIL_ROUTING_CIDRS",
        "",
    ))?);
    if env::args().nth(1).as_deref() == Some("healthcheck") {
        return healthcheck().await;
    }

    #[cfg(feature = "tunnel")]
    let tunnel_configuration = tunnel::configuration()?;
    #[cfg(feature = "tunnel")]
    if tunnel_configuration.is_some() {
        tunnel::install_diagnostics();
    }

    let smtp_address = address(&setting("AHRARA_SMTP_BIND", ":2525"))?;
    let https_address = address(&setting("AHRARA_HTTPS_BIND", ":8443"))?;
    let tls = tls_config()?;
    let sse_enabled = setting("AHRARA_ENABLE_SSE", "0") == "1";
    let mut web_tls = tls.as_ref().clone();
    if sse_enabled {
        web_tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    }

    let smtp_socket = TcpListener::bind(smtp_address).await?;
    let transport_key = secure::load_key(std::path::Path::new(&setting(
        "AHRARA_TRANSPORT_KEY_FILE",
        "/certs/transport.key",
    )))?;
    let limits = limits::RuntimeLimits::from_env()?;
    let broker = Broker::encrypted_with_limits(*transport_key, limits);
    drop(transport_key);
    let sweep = tokio::spawn(broker.clone().sweep());
    let mut router = Router::new()
        .route("/", get(upgrade))
        .route("/healthz", get(|| async { Json(json!({"status": "ok"})) }));
    if sse_enabled {
        router = router.route(
            "/sse",
            get(sse::open).post(sse::send).layer(DefaultBodyLimit::max(
                protocol::MAX_CONTROL + 6 + secure::TAG_LEN,
            )),
        );
    }

    let router = router.with_state(broker.clone());
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
        axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(web_tls)),
    )
    .map(|acceptor| acceptor.acceptor(axum_server::accept::NoDelayAcceptor::new()))
    .handle(handle.clone());
    https
        .http_builder()
        .http1()
        .max_buf_size(16 * 1024)
        .timer(hyper_util::rt::TokioTimer::new())
        .header_read_timeout(Duration::from_secs(10));
    if sse_enabled {
        let streams = setting("AHRARA_HTTP2_MAX_STREAMS", "1024").parse::<u32>()?;
        ensure!(
            (1..=16384).contains(&streams),
            "HTTP/2 stream limit must be 1..16384"
        );

        https
            .http_builder()
            .http2()
            .max_concurrent_streams(streams)
            .max_send_buf_size(128 * 1024);
    }

    let mut web = JoinSet::new();
    let https_router = router.clone();
    web.spawn(async move {
        https
            .serve(https_router.into_make_service())
            .await
            .map_err(anyhow::Error::from)
    });
    #[cfg(feature = "tunnel")]
    if let Some(configuration) = tunnel_configuration {
        let tunnel_router = router.clone();
        let tunnel_broker = broker.clone();
        web.spawn(tunnel::serve(configuration, tunnel_router, tunnel_broker));
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
        prefixes,
    ));
    event(
        "server_started",
        json!({"smtp": smtp_address, "https": https_address}),
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

    #[tokio::test]
    async fn smtp_admission_rejects_excess_sessions_and_idle_timeout_releases_slots() {
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
        let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(rustls::server::ResolvesServerCertUsingSni::new()));
        let task = tokio::spawn(smtp_listener(
            listener,
            broker.clone(),
            TlsAcceptor::from(Arc::new(tls)),
            Arc::new(Vec::new()),
        ));
        let mut first = connect(address, "127.0.0.2").await;

        assert!(greeting(&mut first).await.starts_with("220 "));
        let mut same_source = connect(address, "127.0.0.2").await;

        assert!(!greeting(&mut same_source).await.starts_with("220 "));
        let mut other = connect(address, "127.0.0.3").await;

        assert!(greeting(&mut other).await.starts_with("220 "));
        let mut full = connect(address, "127.0.0.4").await;

        assert!(!greeting(&mut full).await.starts_with("220 "));
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
}
