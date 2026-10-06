//! Cloudflare edge connector for a named Tunnel over HTTP/2.
//!
//! Derived from libcfd 0.2.0 (Apache-2.0, see `LICENSE`) and reduced to what
//! the relay runs: one named Tunnel, the HTTP/2 transport, and WebSocket
//! streams handed straight to the broker. Mirrors cloudflared's
//! `connection/http2.go`: the connector dials the edge over TLS and serves
//! HTTP/2; the first `control-stream` request carries the registration RPC.

use super::{Tunnel, discovery, io::H2Stream};
use crate::broker::Broker;
use anyhow::{Result, bail};
use axum::{
    body::Bytes,
    http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use h2::{
    RecvStream,
    server::{Connection, SendResponse},
};
use libcfd_rpc::{
    RpcClient,
    tunnel::{ClientInformation, ConnectionOptions, ConnectionResponse, TunnelAuth, TunnelClient},
};
use rustls::pki_types::{CertificateDer, ServerName, pem::PemObject};
use serde_json::{Value, json};
use std::{
    net::{IpAddr, SocketAddr},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use tokio::{
    net::TcpStream,
    sync::oneshot,
    task::JoinSet,
    time::{sleep, timeout},
};
use tokio_rustls::{TlsConnector, client::TlsStream};

const SNI: &str = "h2.cftunnel.com";
const UPGRADE: &str = "cf-cloudflared-proxy-connection-upgrade";
const TCP_SOURCE: &str = "cf-cloudflared-proxy-src";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// cloudflared's default `--rpc-timeout`.
const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(5);
/// Framing overhead allowed for buffered small DATA frames per link, about 4,000
/// frames. h2 counts `256 - payload` bytes per small frame until it is read.
const DATA_FRAME_BUDGET: usize = 1 << 20;
/// A link registered this long resets the reconnect backoff.
const HEALTHY: Duration = Duration::from_secs(5);
/// libcfd 0.2.0's registration version, which the edge accepts.
const CLIENT_VERSION: &str = "0.2.0";
/// cloudflared's `features/defaultFeatures`.
const FEATURES: [&str; 5] = [
    "allow_remote_config",
    "serialized_headers",
    "support_datagram_v2",
    "support_quic_eof",
    "management_logs",
];
/// Pushed for locally managed Tunnels; the relay serves every hostname the same way.
const LOCAL_CONFIGURATION: &[u8] =
    br#"{"ingress":[{"hostname":"","service":"http://127.0.0.1:8080"}],"warp-routing":{}}"#;

/// Why one link ended; only a permanent rejection stops the connector.
enum Outcome {
    Stopped,
    Duplicate,
    Permanent,
    Retry,
}

#[derive(Debug, PartialEq)]
enum Kind {
    Http,
    WebSocket,
    Tcp,
    Control,
    Configuration,
}

/// Logs a connection phase with numeric and boolean fields only: never
/// addresses, error strings, headers or credentials.
fn phase(phase: &str, mut fields: Value) {
    fields["phase"] = phase.into();
    crate::event("tunnel_transport", fields);
}

/// Keeps one HA link registered until the broker stops.
pub(super) async fn run(tunnel: Arc<Tunnel>, index: u8, broker: Arc<Broker>) -> Result<()> {
    let link = json!({"connection_index": index});
    let mut attempt = 0u32;
    loop {
        for edge in discovery::edges(tunnel.endpoint.as_deref(), index).await {
            let (outcome, registered) = serve(edge, &tunnel, index, attempt, &broker).await;
            match outcome {
                Outcome::Stopped => return Ok(()),
                Outcome::Permanent => {
                    phase("connector_permanent_error", link);
                    bail!("Tunnel registration rejected");
                }
                Outcome::Duplicate => {
                    phase("connector_duplicate", link.clone());
                    continue;
                }
                Outcome::Retry => phase(
                    "connector_retry",
                    json!({"connection_index": index, "attempt": attempt}),
                ),
            }
            if registered.is_some_and(|at| at.elapsed() >= HEALTHY) {
                attempt = 0;
            }
        }

        attempt = attempt.saturating_add(1);
        tokio::select! {
            _ = broker.stopped.cancelled() => return Ok(()),
            _ = sleep(backoff(attempt)) => {}
        }
    }
}

/// cloudflared's `BackoffHandler`: random within `[0, 1s * 2^min(attempt, 5))`.
fn backoff(attempt: u32) -> Duration {
    Duration::from_nanos(rand::random::<u64>() % (1_000_000_000 << attempt.min(5)))
}

/// Connects, registers and serves one link until it fails or the broker stops.
async fn serve(
    edge: SocketAddr,
    tunnel: &Arc<Tunnel>,
    index: u8,
    attempt: u32,
    broker: &Arc<Broker>,
) -> (Outcome, Option<Instant>) {
    let connected = tokio::select! {
        _ = broker.stopped.cancelled() => return (Outcome::Stopped, None),
        connected = timeout(CONNECT_TIMEOUT, connect(edge, index)) => connected,
    };
    let (mut link, local_ip) = match connected {
        Ok(Ok(connected)) => connected,
        Ok(Err(_)) => return (Outcome::Retry, None),
        Err(_) => {
            phase("connect_timeout", json!({"connection_index": index}));
            return (Outcome::Retry, None);
        }
    };

    let (sender, mut registration) = oneshot::channel();
    let mut sender = Some(sender);
    let mut registered = None;
    let mut control = None;
    let mut streams = JoinSet::new();
    let deadline = sleep(REGISTRATION_TIMEOUT);
    tokio::pin!(deadline);
    let outcome = loop {
        tokio::select! {
            _ = broker.stopped.cancelled() => break Outcome::Stopped,
            _ = streams.join_next(), if !streams.is_empty() => {}
            result = &mut registration, if registered.is_none() => match result {
                Ok(Ok(())) => registered = Some(Instant::now()),
                Ok(Err(outcome)) => break outcome,
                Err(_) => break Outcome::Retry,
            },
            _ = &mut deadline, if registered.is_none() => break Outcome::Retry,
            accepted = link.accept() => match accepted {
                Some(Ok((request, respond))) => match classify(request.headers()) {
                    Kind::Control => match sender.take() {
                        Some(sender) => {
                            control = Some(tokio::spawn(register(
                                request,
                                respond,
                                tunnel.clone(),
                                index,
                                attempt,
                                local_ip.clone(),
                                sender,
                            )));
                        }
                        None => reply(respond, StatusCode::BAD_REQUEST, None),
                    },
                    Kind::WebSocket => {
                        if let Some(session) = websocket(request, respond, broker.clone()) {
                            streams.spawn(session);
                        }
                    }
                    Kind::Configuration => {
                        streams.spawn(configuration(request, respond));
                    }
                    // HTTP through the Tunnel only answers health checks; WSS is the relay transport.
                    Kind::Http if request.method() == Method::GET && request.uri().path() == "/healthz" => {
                        reply(respond, StatusCode::OK, Some(r#"{"status":"ok"}"#));
                    }
                    Kind::Http => reply(respond, StatusCode::NOT_FOUND, None),
                    Kind::Tcp => reply(respond, StatusCode::BAD_GATEWAY, None),
                },
                Some(Err(error)) => {
                    let mut fields = json!({
                        "connection_index": index,
                        "goaway": error.is_go_away(),
                        "remote": error.is_remote(),
                        "io": error.is_io(),
                        "active_streams": streams.len(),
                    });
                    if let Some(reason) = error.reason() {
                        fields["h2_reason"] = u32::from(reason).into();
                    }
                    phase("h2_link_error", fields);
                    break Outcome::Retry;
                }
                None => {
                    phase(
                        "h2_link_closed",
                        json!({"connection_index": index, "active_streams": streams.len()}),
                    );
                    break Outcome::Retry;
                }
            },
        }
    };

    // ponytail: links close without unregisterConnection; the edge detects the loss.
    // Add unregister plus a GOAWAY drain if rolling restarts show edge errors.
    if let Some(control) = control {
        control.abort();
    }
    (outcome, registered)
}

async fn connect(
    edge: SocketAddr,
    index: u8,
) -> Result<(Connection<TlsStream<TcpStream>, Bytes>, Vec<u8>)> {
    let fields = json!({"connection_index": index});
    phase("tcp_connect", fields.clone());
    let tcp = dial(edge).await?;
    // cloudflared registers the local socket IP as `originLocalIp`.
    let local_ip = match tcp.local_addr()?.ip() {
        IpAddr::V4(ip) => ip.octets().to_vec(),
        IpAddr::V6(ip) => ip.octets().to_vec(),
    };
    phase("tls_handshake", fields.clone());
    let tls = TlsConnector::from(tls_config())
        .connect(ServerName::try_from(SNI)?, tcp)
        .await?;
    phase("h2_handshake", fields.clone());
    let link = h2::server::Builder::new()
        .max_concurrent_streams(u32::MAX)
        // One link multiplexes tens of thousands of WebSockets. h2's default budget
        // (~128 small unread DATA frames) closes the whole link during reconnect bursts.
        .data_frame_budget(DATA_FRAME_BUDGET)
        .handshake(tls)
        .await?;
    phase("h2_connected", fields);
    Ok((link, local_ip))
}

async fn dial(edge: SocketAddr) -> std::io::Result<TcpStream> {
    let tcp = TcpStream::connect(edge).await?;
    // Interactive HTTP/2 frames must not wait for Nagle.
    tcp.set_nodelay(true)?;
    Ok(tcp)
}

/// The system store plus Cloudflare's origin CA, like cloudflared's `tlsconfig`.
fn tls_config() -> Arc<rustls::ClientConfig> {
    let system = [
        "/etc/ssl/certs/ca-certificates.crt",
        "/etc/pki/tls/certs/ca-bundle.crt",
        "/etc/ssl/cert.pem",
    ]
    .iter()
    .find_map(|path| std::fs::read(path).ok())
    .unwrap_or_default();
    let mut roots = rustls::RootCertStore::empty();
    for pem in [&system[..], include_bytes!("cloudflare_origin_ca.crt")] {
        for cert in CertificateDer::pem_slice_iter(pem).flatten() {
            let _ = roots.add(cert);
        }
    }

    Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// cloudflared's `determineHTTP2Type`.
fn classify(headers: &HeaderMap) -> Kind {
    match headers.get(UPGRADE).and_then(|value| value.to_str().ok()) {
        Some("update-configuration") => Kind::Configuration,
        Some("websocket") => Kind::WebSocket,
        _ if headers.contains_key(TCP_SOURCE) => Kind::Tcp,
        Some("control-stream") => Kind::Control,
        _ => Kind::Http,
    }
}

/// Registers the link and keeps the RPC stream open while it lives: the edge
/// drops the registration when the control stream ends.
async fn register(
    request: Request<RecvStream>,
    mut respond: SendResponse<Bytes>,
    tunnel: Arc<Tunnel>,
    index: u8,
    attempt: u32,
    local_ip: Vec<u8>,
    registered: oneshot::Sender<Result<(), Outcome>>,
) {
    let result = async {
        let send = respond
            .send_response(Response::new(()), false)
            .map_err(|_| Outcome::Retry)?;
        let mut client =
            TunnelClient::new(RpcClient::new(H2Stream::new(request.into_body(), send)));
        let auth = TunnelAuth {
            account_tag: tunnel.account_tag.clone(),
            tunnel_secret: tunnel.secret.clone(),
        };
        let response = async {
            client.bootstrap().await?;
            client
                .register_connection(auth, &tunnel.id, index, &options(local_ip, attempt))
                .await
        }
        .await
        .map_err(|_| Outcome::Retry)?;
        match response {
            ConnectionResponse::Details(details) => {
                if !details.tunnel_is_remotely_managed {
                    let _ = client.update_local_configuration(LOCAL_CONFIGURATION).await;
                }
                Ok(client)
            }
            ConnectionResponse::Error(error) => {
                let _ = client.close().await;
                Err(if error.cause == "EDUPCONN" {
                    Outcome::Duplicate
                } else if error.should_retry {
                    Outcome::Retry
                } else {
                    Outcome::Permanent
                })
            }
        }
    }
    .await;

    match result {
        Ok(client) => {
            phase("h2_registered", json!({"connection_index": index}));
            let _ = registered.send(Ok(()));
            let _client = client;
            std::future::pending::<()>().await;
        }
        Err(outcome) => {
            let _ = registered.send(Err(outcome));
        }
    }
}

fn options(origin_local_ip: Vec<u8>, attempt: u32) -> ConnectionOptions {
    // cloudflared keeps one connector ID per process for every link.
    static CLIENT: OnceLock<[u8; 16]> = OnceLock::new();
    ConnectionOptions {
        client: ClientInformation {
            client_identifier: CLIENT.get_or_init(rand::random).to_vec(),
            features: FEATURES.map(String::from).to_vec(),
            version: CLIENT_VERSION.into(),
            arch: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
        },
        origin_local_ip,
        replace_existing: false,
        compression_quality: 0,
        number_previous_attempts: attempt.min(u8::MAX.into()) as u8,
    }
}

/// Answers a WebSocket upgrade and returns its session. The request headers are
/// dropped here, so the long-lived session task never stores them.
fn websocket(
    request: Request<RecvStream>,
    mut respond: SendResponse<Bytes>,
    broker: Arc<Broker>,
) -> Option<impl Future<Output = ()>> {
    let (parts, body) = request.into_parts();
    let accept = match super::handshake(&parts) {
        Ok(accept) => accept,
        Err(status) => {
            reply(respond, status, None);
            return None;
        }
    };
    let Some(client) = broker
        .admission
        .try_client(crate::client_ip(&parts.headers))
    else {
        reply(respond, StatusCode::TOO_MANY_REQUESTS, None);
        return None;
    };
    let head = head(
        StatusCode::SWITCHING_PROTOCOLS,
        &[
            ("connection", "Upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-accept", &accept),
        ],
    );
    let send = respond.send_response(head, false).ok()?;
    Some(super::serve_stream(
        broker,
        H2Stream::new(body, send),
        client,
    ))
}

/// Acknowledges remote configuration pushes; the relay has no ingress rules to apply.
async fn configuration(request: Request<RecvStream>, mut respond: SendResponse<Bytes>) {
    #[derive(serde::Deserialize)]
    struct Update {
        version: i32,
    }

    let mut body = request.into_body();
    let mut data = Vec::new();
    while let Some(Ok(chunk)) = body.data().await {
        let _ = body.flow_control().release_capacity(chunk.len());
        data.extend_from_slice(&chunk);
    }
    let reply = match serde_json::from_slice::<Update>(&data) {
        Ok(update) => json!({"latestAppliedVersion": update.version, "err": ""}),
        Err(_) => json!({"latestAppliedVersion": -1, "err": "invalid configuration"}),
    };
    if let Ok(mut send) = respond.send_response(Response::new(()), false) {
        let _ = send.send_data(reply.to_string().into(), true);
    }
}

/// Sends a complete response with an optional JSON body.
fn reply(mut respond: SendResponse<Bytes>, status: StatusCode, json: Option<&'static str>) {
    let Some(json) = json else {
        let _ = respond.send_response(head(status, &[]), true);
        return;
    };
    let length = json.len().to_string();
    let head = head(
        status,
        &[
            ("content-type", "application/json"),
            ("content-length", &length),
        ],
    );
    if let Ok(mut send) = respond.send_response(head, false) {
        let _ = send.send_data(Bytes::from_static(json.as_bytes()), true);
    }
}

/// cloudflared's response encoding: user headers travel base64-serialized in
/// one header (present even when empty), `content-length` also as itself,
/// and HTTP/2's missing 101 becomes 200.
fn head(status: StatusCode, headers: &[(&str, &str)]) -> Response<()> {
    let mut response = Response::new(());
    *response.status_mut() = if status == StatusCode::SWITCHING_PROTOCOLS {
        StatusCode::OK
    } else {
        status
    };
    let serialized = headers
        .iter()
        .map(|(name, value)| format!("{}:{}", STANDARD.encode(name), STANDARD.encode(value)))
        .collect::<Vec<_>>()
        .join(";");
    let map = response.headers_mut();
    for (name, value) in headers {
        if *name == "content-length" {
            map.insert("content-length", HeaderValue::from_str(value).unwrap());
        }
    }
    // Base64 and its separators are always valid header bytes.
    map.insert(
        "cf-cloudflared-response-headers",
        HeaderValue::from_str(&serialized).unwrap(),
    );
    map.insert(
        "cf-cloudflared-response-meta",
        HeaderValue::from_static(r#"{"src":"origin"}"#),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{protocol as p, secure};
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use ed25519_dalek::{Signer, SigningKey};
    use futures_util::{SinkExt, StreamExt};
    use tokio::io::AsyncReadExt;
    use tokio_tungstenite::{
        WebSocketStream,
        tungstenite::{Message, protocol::Role},
    };
    use tokio_util::compat::FuturesAsyncReadCompatExt;

    fn user_headers(response: &Response<RecvStream>) -> Vec<(String, String)> {
        let serialized = response.headers()["cf-cloudflared-response-headers"]
            .to_str()
            .unwrap();
        serialized
            .split(';')
            .filter(|pair| !pair.is_empty())
            .map(|pair| {
                let (name, value) = pair.split_once(':').unwrap();
                let decode = |part| String::from_utf8(STANDARD.decode(part).unwrap()).unwrap();
                (decode(name), decode(value))
            })
            .collect()
    }

    #[tokio::test]
    async fn websocket_admission_counts_cloudflare_client_addresses() {
        let broker = Broker::encrypted_with_limits(
            [7; 32],
            crate::limits::RuntimeLimits {
                clients_per_source: 1,
                ..crate::limits::RuntimeLimits::default()
            },
        );
        let (edge_io, relay_io) = tokio::io::duplex(64 * 1024);
        let (edge, relay) = tokio::join!(
            h2::client::handshake(edge_io),
            h2::server::handshake(relay_io)
        );
        let (mut edge, edge_link) = edge.unwrap();
        tokio::spawn(edge_link);
        let mut relay = relay.unwrap();
        let relay_broker = broker.clone();
        tokio::spawn(async move {
            let mut streams = JoinSet::new();
            while let Some(Ok((request, respond))) = relay.accept().await {
                if let Some(session) = websocket(request, respond, relay_broker.clone()) {
                    streams.spawn(session);
                }
            }
        });
        let mut held = Vec::new();

        for (client, status) in [
            ("192.0.2.1", StatusCode::OK),
            ("192.0.2.1", StatusCode::TOO_MANY_REQUESTS),
            ("192.0.2.2", StatusCode::OK),
        ] {
            let request = Request::get("https://ahrara.dev/")
                .header(UPGRADE, "websocket")
                .header("sec-websocket-version", "13")
                .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==") // gitleaks:allow: RFC 6455 example nonce.
                .header("cf-connecting-ip", client)
                .body(())
                .unwrap();
            let (response, send) = edge.send_request(request, false).unwrap();

            assert_eq!(response.await.unwrap().status(), status, "{client}");
            // Open streams keep their sessions, and so their slots, alive.
            held.push(send);
        }
    }

    #[tokio::test]
    async fn websocket_stream_requires_signed_authentication_and_cleans_up_on_edge_loss() {
        let transport = snow::Builder::new(secure::PATTERN.parse().unwrap())
            .generate_keypair()
            .unwrap();
        for valid in [true, false] {
            let broker = Broker::encrypted(transport.private.as_slice().try_into().unwrap());
            let (edge_io, relay_io) = tokio::io::duplex(64 * 1024);
            let (edge, relay) = tokio::join!(
                h2::client::handshake(edge_io),
                h2::server::handshake(relay_io)
            );
            let (mut edge, edge_link) = edge.unwrap();
            tokio::spawn(edge_link);
            let mut relay = relay.unwrap();
            let relay_broker = broker.clone();
            // Like `serve`: losing the link drops the JoinSet and aborts its streams.
            let relay_task = tokio::spawn(async move {
                let mut streams = JoinSet::new();
                while let Some(Ok((request, respond))) = relay.accept().await {
                    assert_eq!(classify(request.headers()), Kind::WebSocket);
                    if let Some(session) = websocket(request, respond, relay_broker.clone()) {
                        streams.spawn(session);
                    }
                }
            });
            let request = Request::get("https://ahrara.dev/")
                .header(UPGRADE, "websocket")
                .header("sec-websocket-version", "13")
                .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==") // gitleaks:allow: RFC 6455 example nonce.
                .body(())
                .unwrap();
            let (response, send) = edge.send_request(request, false).unwrap();
            let response = response.await.unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            assert!(user_headers(&response).contains(&(
                "sec-websocket-accept".into(),
                "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=".into()
            )));
            let stream = H2Stream::new(response.into_body(), send).compat();
            let mut socket = WebSocketStream::from_raw_socket(stream, Role::Client, None).await;
            let mut handshake =
                secure::initiator(transport.public.as_slice().try_into().unwrap()).unwrap();
            let mut message = [0; secure::MAX_HANDSHAKE];
            let length = handshake.write_message(&[], &mut message).unwrap();
            socket
                .send(Message::Binary(message[..length].to_vec().into()))
                .await
                .unwrap();
            let Message::Binary(raw) = socket.next().await.unwrap().unwrap() else {
                panic!("binary handshake required");
            };

            assert_eq!(handshake.read_message(&raw, &mut message).unwrap(), 0);
            let binding: [u8; 32] = handshake.get_handshake_hash().try_into().unwrap();
            let (mut sender, mut receiver) = secure::into_session(handshake).unwrap();
            let key = SigningKey::from_bytes(&[9; 32]);
            let public = key.verifying_key().to_bytes();
            let hello = p::json(
                p::HELLO,
                &json!({"public_key": URL_SAFE_NO_PAD.encode(public)}),
            )
            .unwrap();
            socket
                .send(Message::Binary(sender.seal(&hello).unwrap().into()))
                .await
                .unwrap();
            let Message::Binary(raw) = socket.next().await.unwrap().unwrap() else {
                panic!("binary challenge required");
            };
            let raw = receiver.open(&raw).unwrap();
            let (kind, payload) = p::decode(&raw).unwrap();

            assert_eq!(kind, p::CHALLENGE);
            let value: Value = serde_json::from_slice(payload).unwrap();
            let challenge: [u8; 32] = URL_SAFE_NO_PAD
                .decode(value["challenge"].as_str().unwrap())
                .unwrap()
                .try_into()
                .unwrap();
            let mut signature = key
                .sign(&secure::auth_preimage(&public, &challenge, &binding))
                .to_bytes();
            if !valid {
                signature[0] ^= 1;
            }

            let authenticate = p::json(
                p::AUTHENTICATE,
                &json!({"signature": URL_SAFE_NO_PAD.encode(signature)}),
            )
            .unwrap();
            socket
                .send(Message::Binary(sender.seal(&authenticate).unwrap().into()))
                .await
                .unwrap();
            let Message::Binary(raw) = socket.next().await.unwrap().unwrap() else {
                panic!("binary result required");
            };

            assert_eq!(
                p::decode(&receiver.open(&raw).unwrap()).unwrap().0,
                if valid { p::READY } else { p::FAILURE }
            );
            assert_eq!(broker.lookup(&p::derive_route(&public)).is_some(), valid);
            if valid {
                socket
                    .send(Message::Ping(Bytes::from_static(b"alive")))
                    .await
                    .unwrap();
                assert_eq!(
                    socket.next().await.unwrap().unwrap(),
                    Message::Pong(Bytes::from_static(b"alive"))
                );
            } else {
                assert!(
                    timeout(Duration::from_secs(1), socket.next())
                        .await
                        .unwrap()
                        .is_none_or(|message| message.is_err())
                );
            }

            relay_task.abort();
            timeout(Duration::from_secs(1), async {
                while broker.lookup(&p::derive_route(&public)).is_some() {
                    sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("edge loss must remove the Identity");
        }
    }

    #[test]
    fn classifies_edge_streams_like_cloudflared() {
        let headers = |pairs: &[(&'static str, &'static str)]| {
            pairs
                .iter()
                .map(|(name, value)| (name.parse().unwrap(), HeaderValue::from_static(value)))
                .collect::<HeaderMap>()
        };

        assert_eq!(classify(&headers(&[])), Kind::Http);
        assert_eq!(
            classify(&headers(&[(UPGRADE, "websocket")])),
            Kind::WebSocket
        );
        assert_eq!(
            classify(&headers(&[(UPGRADE, "control-stream")])),
            Kind::Control
        );
        assert_eq!(
            classify(&headers(&[(UPGRADE, "update-configuration")])),
            Kind::Configuration
        );
        assert_eq!(classify(&headers(&[(TCP_SOURCE, "127.0.0.1")])), Kind::Tcp);
        assert_eq!(
            classify(&headers(&[
                (UPGRADE, "control-stream"),
                (TCP_SOURCE, "127.0.0.1")
            ])),
            Kind::Tcp
        );
    }

    #[test]
    fn response_head_follows_cloudflared_header_rules() {
        let empty = head(StatusCode::NOT_FOUND, &[]);

        assert_eq!(empty.status(), StatusCode::NOT_FOUND);
        assert_eq!(empty.headers()["cf-cloudflared-response-headers"], "");
        assert_eq!(
            empty.headers()["cf-cloudflared-response-meta"],
            r#"{"src":"origin"}"#
        );
        let upgrade = head(
            StatusCode::SWITCHING_PROTOCOLS,
            &[("upgrade", "websocket"), ("content-length", "0")],
        );

        assert_eq!(upgrade.status(), StatusCode::OK);
        assert_eq!(upgrade.headers()["content-length"], "0");
        assert_eq!(
            upgrade.headers()["cf-cloudflared-response-headers"],
            "dXBncmFkZQ==:d2Vic29ja2V0;Y29udGVudC1sZW5ndGg=:MA=="
        );
    }

    #[test]
    fn backoff_stays_within_cloudflared_bounds() {
        for attempt in [1, 3, 5, 9, u32::MAX] {
            assert!(backoff(attempt) < Duration::from_secs(1 << attempt.min(5)));
        }
    }

    #[tokio::test]
    async fn edge_socket_disables_nagle_and_times_out_a_silent_tls_peer() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        assert!(dial(address).await.unwrap().nodelay().unwrap());
        let _ = listener.accept().await.unwrap();
        let connecting = timeout(Duration::from_millis(250), connect(address, 3));
        tokio::pin!(connecting);
        let (mut peer, _) = tokio::select! {
            _ = &mut connecting => panic!("connection finished before the peer accepted"),
            accepted = listener.accept() => accepted.unwrap(),
        };
        let mut hello = [0u8; 1];
        tokio::select! {
            _ = &mut connecting => panic!("connection finished before the TLS ClientHello"),
            received = peer.read_exact(&mut hello) => received.unwrap(),
        };

        assert_eq!(
            hello[0], 0x16,
            "the edge must receive a TLS handshake record"
        );
        assert!(connecting.await.is_err(), "a silent TLS peer must time out");
    }
}
