use crate::{
    core::Core,
    protocol::*,
    session::{Session, Step},
};
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use rand::Rng;
use rustls::pki_types::{CertificateDer, pem::PemObject};
use std::{sync::Arc, time::Duration};
use tokio::net::TcpStream;
use tokio::time::Instant;
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, connect_async_tls_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};
use tokio_util::sync::CancellationToken;

type Wire = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub async fn run(core: Arc<Core>, cancel: CancellationToken) {
    if core.config.server_public_key.is_none() {
        core.set_state(
            "failed",
            Some("server_public_key is required for encrypted transport".into()),
        )
        .await;

        return;
    }

    let mut attempt = 0u32;
    let mut previously_connected = false;

    loop {
        let wire = tokio::select! {
            _ = cancel.cancelled() => break,
            result = open_wire(&core) => result,
        };
        let result = match wire {
            Ok(mut wire) => {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        // Keep ownership until the relay closes its side. Dropping
                        // TCP alone races immediate reuse of this identity.
                        let ws = &mut wire;
                        let _ = tokio::time::timeout(Duration::from_secs(5), async {
                            ws.close(None).await?;
                            while let Some(frame) = ws.next().await {
                                frame?;
                            }
                            Ok::<(), tokio_tungstenite::tungstenite::Error>(())
                        }).await;
                        break;
                    }
                    result = connect(&core, &mut wire) => result,
                }
            }
            Err(error) => Err(error),
        };
        if core.state.borrow().connected {
            attempt = 0;
            previously_connected = true;
        }

        let error = result
            .err()
            .map(|e| e.to_string())
            .unwrap_or_else(|| "connection closed".into());
        // After a network partition the old TCP socket can outlive this client's
        // PONG deadline. Retry with backoff until the server expires it; never
        // request takeover. An initially occupied Identity still fails clearly.
        if error == "authentication_failed" || (error == "identity_in_use" && !previously_connected)
        {
            core.set_state("failed", Some(error)).await;
            return;
        }

        core.set_state("reconnecting", Some(error)).await;
        let base = (1u64 << attempt.min(5)).min(30);
        let jitter = rand::thread_rng().gen_range(0.8..1.0);
        let delay = Duration::from_secs_f64(base as f64 * jitter);
        attempt = attempt.saturating_add(1);
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(delay) => {}
        }
    }

    core.set_state("stopped", None).await;
}

async fn send_packet(wire: &mut Wire, frame: Vec<u8>) -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(30),
        wire.send(Message::Binary(frame.into())),
    )
    .await??;

    Ok(())
}

struct Heartbeat {
    next: Instant,
    pending: Option<[u8; 8]>,
    interval: Duration,
    timeout: Duration,
}

impl Heartbeat {
    fn new(interval: Duration, timeout: Duration) -> Self {
        Self {
            next: Instant::now() + Self::delay(interval),
            pending: None,
            interval,
            timeout,
        }
    }

    fn delay(interval: Duration) -> Duration {
        interval.mul_f64(rand::thread_rng().gen_range(0.8..=1.2))
    }
}

async fn receive_packet(socket: &mut Wire, heartbeat: &mut Heartbeat) -> Result<Vec<u8>> {
    loop {
        let frame = tokio::select! {
            // A stream of data or unrelated PONGs must not starve the deadline.
            biased;
            _ = tokio::time::sleep_until(heartbeat.next) => {
                if heartbeat.pending.is_some() { bail!("heartbeat PONG timeout") }
                let payload = rand::random::<[u8; 8]>();
                tokio::time::timeout(heartbeat.timeout, socket.send(Message::Ping(payload.to_vec().into()))).await??;
                heartbeat.pending = Some(payload);
                heartbeat.next = Instant::now() + heartbeat.timeout;
                continue;
            }
            frame = socket.next() => frame.context("connection closed")??,
        };

        match frame {
            Message::Binary(bytes) => return Ok(bytes.to_vec()),
            Message::Ping(bytes) => {
                tokio::time::timeout(heartbeat.timeout, socket.send(Message::Pong(bytes))).await??
            }
            Message::Pong(bytes) => {
                if heartbeat
                    .pending
                    .as_ref()
                    .is_some_and(|expected| bytes.as_ref() == expected)
                {
                    heartbeat.pending = None;
                    heartbeat.next = Instant::now() + Heartbeat::delay(heartbeat.interval);
                }
            }
            Message::Close(_) => bail!("connection closed"),
            _ => bail!("binary protocol required"),
        }
    }
}

fn tls_config(core: &Core) -> Result<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(path) = &core.config.tls_ca_path {
        let certs = CertificateDer::pem_file_iter(path)
            .context("cannot open configured TLS CA")?
            .collect::<Result<Vec<_>, _>>()?;
        if certs.is_empty() {
            bail!("configured TLS CA contains no certificates")
        }

        for cert in certs {
            roots.add(cert)?;
        }
    }

    Ok(rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth())
}

async fn open_wire(core: &Core) -> Result<Wire> {
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME))
        .max_frame_size(Some(MAX_FRAME))
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_FRAME * 2);
    let connector = Connector::Rustls(Arc::new(tls_config(core)?));
    let (socket, _) = tokio::time::timeout(
        Duration::from_secs(10),
        connect_async_tls_with_config(&core.config.server_url, Some(config), true, Some(connector)),
    )
    .await??;

    Ok(socket)
}

async fn connect(core: &Arc<Core>, wire: &mut Wire) -> Result<()> {
    let pin = crate::secure::parse_public_key(
        core.config
            .server_public_key
            .as_deref()
            .context("server_public_key is required for encrypted transport")?,
    )?;
    let mut heartbeat = Heartbeat::new(Duration::from_secs(90), Duration::from_secs(30));
    let (mut session, handshake) = Session::open(core.identity.clone(), &pin)?;
    tokio::time::timeout(Duration::from_secs(15), async {
        send_packet(wire, handshake).await?;
        loop {
            let packet = receive_packet(wire, &mut heartbeat).await?;
            match session.receive(&packet)? {
                Step::Send(bytes) => send_packet(wire, bytes).await?,
                Step::Work { challenge, bits } => {
                    let nonce =
                        tokio::task::spawn_blocking(move || solve_work(&challenge, bits)).await?;
                    send_packet(wire, session.work(nonce)?).await?;
                }
                Step::Ready => return Ok::<(), anyhow::Error>(()),
                _ => bail!("authentication_failed"),
            }
        }
    })
    .await??;
    core.set_state("connected", None).await;

    loop {
        let packet = receive_packet(wire, &mut heartbeat).await?;
        match session.receive(&packet)? {
            Step::Send(bytes) => send_packet(wire, bytes).await?,
            Step::Offer { recipient } => {
                let accept = core
                    .storage
                    .accepts(core.identity.fingerprint(), recipient)
                    .await?;
                send_packet(wire, session.offer(accept)?).await?;
            }
            Step::Deliver {
                transfer_id,
                recipients,
                sender,
                raw,
            } => {
                let saved = core
                    .store_delivery(transfer_id, recipients, sender, raw)
                    .await;
                let stored = saved.is_ok();
                if let Ok(emails) = saved {
                    core.received(emails);
                }

                send_packet(wire, session.stored(stored)?).await?;
            }
            Step::Idle => {}
            Step::Work { .. } | Step::Ready => bail!("unexpected protocol frame"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

    #[tokio::test]
    async fn cancellation_waits_for_the_peer_to_finish_closing() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (started, connected) = tokio::sync::oneshot::channel();
        let (closing, close_received) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();

            assert!(matches!(ws.next().await, Some(Ok(Message::Binary(_)))));
            started.send(()).unwrap();

            assert!(
                matches!(ws.next().await, Some(Ok(Message::Close(_)))),
                "cancellation must send WebSocket CLOSE before dropping TCP"
            );
            let _ = ws.flush().await;
            closing.send(()).unwrap();
            // The relay has observed CLOSE but still owns the identity until it
            // finishes its cleanup and drops the connection.
            released.await.unwrap();
        });
        let directory = tempfile::tempdir().unwrap();
        let core = Core::open_temporary(
            crate::config::Config {
                server_url: format!("ws://{address}"),
                server_public_key: Some(format!(
                    "ahrara_server_v1_{}",
                    URL_SAFE_NO_PAD.encode([7; 32])
                )),
                database_path: directory.path().join("inbox.db"),
                identity_path: directory.path().join("identity.key"),
                ..crate::config::Config::default()
            },
            directory,
        )
        .unwrap();
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(run(core, cancel.clone()));
        connected.await.unwrap();
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), close_received)
            .await
            .expect("client did not initiate a close handshake")
            .expect("client dropped TCP without closing");

        assert!(
            !worker.is_finished(),
            "shutdown completed before the relay released the connection"
        );
        release.send(()).unwrap();
        server.await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn heartbeat_detects_blackhole_and_abrupt_tcp_close() {
        for mode in [0, 1, 2] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let peer = tokio::spawn(async move {
                let (tcp, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
                let Message::Ping(mut payload) = ws.next().await.unwrap().unwrap() else {
                    panic!("expected native PING")
                };
                if mode == 2 {
                    let mut wrong = payload.to_vec();
                    wrong[0] ^= 1;
                    payload = wrong.into();
                    // Replace the library's queued automatic reply with an
                    // unrelated PONG; this must not satisfy our pending PING.
                    ws.send(Message::Pong(payload)).await.unwrap();
                }
                if mode != 1 {
                    // Keep TCP open, but do not flush the library's queued PONG.
                    // This models a stalled process / application-level packet loss.
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
                // Drop TCP without a WebSocket CLOSE handshake.
            });
            let tcp = TcpStream::connect(address).await.unwrap();
            let (socket, _) = tokio_tungstenite::client_async(
                format!("ws://{address}"),
                MaybeTlsStream::Plain(tcp),
            )
            .await
            .unwrap();
            let mut socket = socket;
            let mut heartbeat =
                Heartbeat::new(Duration::from_millis(30), Duration::from_millis(60));
            let error = tokio::time::timeout(
                Duration::from_secs(2),
                receive_packet(&mut socket, &mut heartbeat),
            )
            .await
            .unwrap()
            .unwrap_err();
            if mode != 1 {
                assert!(error.to_string().contains("PONG timeout"), "{error}");
            }

            peer.await.unwrap();
        }
    }

    #[tokio::test]
    #[ignore = "real native heartbeat interval, up to 110 seconds"]
    async fn native_heartbeat_round_trip() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let Message::Ping(payload) = ws.next().await.unwrap().unwrap() else {
                panic!("client did not send native PING")
            };
            ws.send(Message::Pong(payload)).await.unwrap();
            ws.send(Message::Binary(vec![1, 2, 3].into()))
                .await
                .unwrap();

            tokio::time::sleep(Duration::from_secs(1)).await;
        });
        let tcp = TcpStream::connect(address).await.unwrap();
        let (socket, _) =
            tokio_tungstenite::client_async(format!("ws://{address}"), MaybeTlsStream::Plain(tcp))
                .await
                .unwrap();
        let mut socket = socket;
        let mut heartbeat = Heartbeat::new(Duration::from_secs(90), Duration::from_secs(30));
        let received = tokio::time::timeout(
            Duration::from_secs(110),
            receive_packet(&mut socket, &mut heartbeat),
        )
        .await;

        assert_eq!(
            received.expect("client never initiated heartbeat").unwrap(),
            vec![1, 2, 3]
        );

        peer.await.unwrap();
    }
}
