//! Optional named Cloudflare Tunnel, serving the broker inside the Rust process.
use crate::{
    broker::{Broker, Connection},
    protocol as p,
};
use anyhow::{Context, Result, anyhow, ensure};
use axum::{
    Router,
    http::{HeaderMap, Method, StatusCode},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use libcfd::{
    AxumOrigin, Body, EdgeConnector, EdgeOptions, NamedTunnel, Origin, Request, Response,
    StreamOrigin, Tunnel, WebSocketResponder,
};
use std::{env, sync::Arc, time::Duration};
use tokio::task::JoinSet;
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{protocol::Role, protocol::WebSocketConfig},
};
use tokio_util::{
    compat::{Compat, FuturesAsyncReadCompatExt},
    sync::CancellationToken,
};

pub type Socket = WebSocketStream<Compat<libcfd::Stream>>;

#[derive(Default)]
struct TransportFields(serde_json::Map<String, serde_json::Value>);

impl TransportFields {
    fn value(&mut self, name: &str, value: serde_json::Value) {
        if matches!(
            name,
            "connection_index"
                | "attempt"
                | "h2_reason"
                | "goaway"
                | "remote"
                | "io"
                | "active_streams"
        ) {
            self.0.insert(name.into(), value);
        }
    }
}

impl tracing::field::Visit for TransportFields {
    fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {
        // Never format error strings, headers, tokens, URLs or RPC server text.
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.value(field.name(), value.into());
    }
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.value(field.name(), value.into());
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "event_kind"
            && matches!(
                value,
                "h2_registered"
                    | "tcp_connect"
                    | "tls_handshake"
                    | "h2_handshake"
                    | "h2_connected"
                    | "connect_timeout"
                    | "connector_duplicate"
                    | "h2_link_closed"
                    | "h2_link_error"
                    | "connector_retry"
                    | "connector_permanent_error"
            )
        {
            self.0.insert("phase".into(), value.into());
        }
    }
}

struct TransportDiagnostics;

impl tracing::Subscriber for TransportDiagnostics {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.level() <= &tracing::Level::INFO
            && (metadata.target().starts_with("libcfd::edge::h2")
                || metadata.target().starts_with("libcfd::edge::connector"))
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = TransportFields::default();
        event.record(&mut fields);
        if fields.0.contains_key("phase") {
            crate::event("tunnel_transport", fields.0.into());
        }
    }
}

pub fn install_diagnostics() {
    let _ = tracing::subscriber::set_global_default(TransportDiagnostics);
}

pub struct Configuration {
    identity: NamedTunnel,
    options: EdgeOptions,
    connections: u8,
}

pub fn configuration() -> Result<Option<Configuration>> {
    let Some(path) = env::var("AHRARA_TUNNEL_TOKEN_FILE")
        .ok()
        .filter(|p| !p.is_empty())
    else {
        return Ok(None);
    };
    let token = zeroize::Zeroizing::new(
        std::fs::read_to_string(path).context("cannot read Tunnel token file")?,
    );
    // Parsing errors must never include token fragments or account information.
    let identity =
        NamedTunnel::from_token(token.trim()).map_err(|_| anyhow!("invalid Tunnel token"))?;
    let connections = crate::setting("AHRARA_TUNNEL_CONNECTIONS", "4").parse::<u8>()?;
    ensure!(
        (1..=4).contains(&connections),
        "Tunnel connection count must be 1..4"
    );
    let options = EdgeOptions {
        grace_period: Duration::from_secs(5),
        ..Default::default()
    };
    ensure!(
        env::var_os("AHRARA_TUNNEL_EDGE_ADDRESS").is_none()
            && env::var_os("AHRARA_TUNNEL_CA_FILE").is_none()
            && env::var_os("AHRARA_TUNNEL_BRIDGE").is_none(),
        "Tunnel test overrides are not supported"
    );
    Ok(Some(Configuration {
        identity,
        options,
        connections,
    }))
}

fn handshake(request: &Request) -> Response {
    let status = if request.uri.path() != "/" {
        StatusCode::NOT_FOUND
    } else if request
        .headers
        .get("origin")
        .is_some_and(|value| !value.is_empty())
    {
        StatusCode::FORBIDDEN
    } else if request.method != Method::GET
        || request
            .headers
            .get("sec-websocket-version")
            .is_none_or(|value| value != "13")
        || request
            .headers
            .get("sec-websocket-key")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| STANDARD.decode(value).ok())
            .is_none_or(|key| key.len() != 16)
    {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::SWITCHING_PROTOCOLS
    };
    let mut headers = HeaderMap::new();
    if status == StatusCode::SWITCHING_PROTOCOLS {
        let key = request.headers["sec-websocket-key"].to_str().unwrap();
        headers.insert(
            "sec-websocket-accept",
            libcfd::websocket_accept(key).parse().unwrap(),
        );
        headers.insert("connection", "Upgrade".parse().unwrap());
        headers.insert("upgrade", "websocket".parse().unwrap());
    }
    Response::new(status, headers, Body::empty())
}

async fn serve_stream(broker: Arc<Broker>, stream: libcfd::Stream) {
    let config = WebSocketConfig::default()
        .read_buffer_size(512)
        .write_buffer_size(0)
        .max_write_buffer_size(p::MAX_FRAME * 2)
        .max_message_size(Some(p::MAX_CONTROL + 6 + crate::secure::TAG_LEN))
        .max_frame_size(Some(p::MAX_CONTROL + 6 + crate::secure::TAG_LEN));
    let socket =
        WebSocketStream::from_raw_socket(stream.compat(), Role::Server, Some(config)).await;
    broker
        .serve_connection(
            Connection::Tunnel(Box::new(socket)),
            CancellationToken::new(),
        )
        .await;
}

struct WebSocketOrigin {
    broker: Arc<Broker>,
}

impl StreamOrigin<WebSocketResponder> for WebSocketOrigin {
    fn connect(&self, request: Request, respond: WebSocketResponder) {
        let response = handshake(&request);
        let broker = self.broker.clone();
        respond.serve(response, move |stream| serve_stream(broker, stream));
    }
}

pub async fn serve(config: Configuration, router: Router, broker: Arc<Broker>) -> Result<()> {
    let mut tasks = JoinSet::new();
    for index in 0..config.connections {
        let identity = config.identity.clone();
        let origin =
            Origin::http(AxumOrigin::new(router.clone())).with_websocket(WebSocketOrigin {
                broker: broker.clone(),
            });
        let mut options = config.options.clone();
        options.connection_index = index;
        options.http_copy_buffer_size = 512;
        let stopped = broker.stopped.clone();
        tasks.spawn(async move {
            EdgeConnector::new(options)
                .run(Tunnel::named(identity), origin, stopped.cancelled_owned())
                .await
        });
    }
    crate::event(
        "tunnel_started",
        serde_json::json!({"connections": config.connections, "transport": "http2"}),
    );
    // Retryable failures remain inside the SDK; a permanent registration failure stops the broker.
    while let Some(result) = tasks.join_next().await {
        result?.map_err(|_| anyhow!("Tunnel connector stopped"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{auth, protocol as p};
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use bytes::Bytes;
    use ed25519_dalek::{Signer, SigningKey};
    use futures_util::{SinkExt, StreamExt};
    use tokio::io::duplex;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_util::compat::TokioAsyncReadCompatExt;

    #[test]
    fn transport_diagnostics_drop_untrusted_fields() {
        use tracing::callsite::Callsite as _;
        use tracing::field::Value;
        static __CALLSITE: tracing::callsite::DefaultCallsite = tracing::callsite2! {
            name: "transport-fields-test", kind: tracing::metadata::Kind::EVENT,
            target: "libcfd::edge::h2", level: tracing::Level::WARN,
            fields: h2_reason, remote, error, token, event_kind, connection_index, address,
        };
        let metadata = __CALLSITE.metadata();
        let names = metadata.fields();
        let field_names = [
            "h2_reason",
            "remote",
            "error",
            "token",
            "event_kind",
            "connection_index",
            "address",
        ]
        .map(|name| names.field(name).unwrap());
        let reason = Some(7u32);
        for phase in [
            "h2_registered",
            "tcp_connect",
            "tls_handshake",
            "h2_handshake",
            "h2_connected",
            "connect_timeout",
            "connector_duplicate",
            "credential",
        ] {
            let values: [(_, Option<&dyn Value>); 7] = [
                (&field_names[0], Some(&reason)),
                (&field_names[1], Some(&true)),
                (&field_names[2], Some(&"private server text")),
                (&field_names[3], Some(&"credential")),
                (&field_names[4], Some(&phase)),
                (&field_names[5], Some(&3u64)),
                (&field_names[6], Some(&"private destination")),
            ];
            let values = names.value_set(&values);
            let event = tracing::Event::new(metadata, &values);
            let mut fields = TransportFields::default();
            event.record(&mut fields);
            let mut expected =
                serde_json::json!({"h2_reason":7, "remote":true, "connection_index":3})
                    .as_object()
                    .unwrap()
                    .clone();
            if phase != "credential" {
                expected.insert("phase".into(), phase.into());
            }
            assert_eq!(fields.0, expected);
        }
    }

    #[tokio::test]
    async fn raw_tunnel_requires_signed_authentication_and_cleans_up_on_edge_loss() {
        for valid in [true, false] {
            let broker = Broker::new();
            let (client, server) = duplex(512);
            let task = tokio::spawn(serve_stream(
                broker.clone(),
                libcfd::Stream::from_io(server.compat()),
            ));
            let mut socket = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
            let key = SigningKey::from_bytes(&[9; 32]);
            let public = key.verifying_key().to_bytes();
            socket
                .send(Message::Binary(
                    p::json(
                        p::HELLO,
                        &serde_json::json!({"public_key": URL_SAFE_NO_PAD.encode(public)}),
                    )
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();
            let Message::Binary(raw) = socket.next().await.unwrap().unwrap() else {
                panic!("binary challenge required");
            };
            let (kind, payload) = p::decode(&raw).unwrap();
            assert_eq!(kind, p::CHALLENGE);
            let value: serde_json::Value = serde_json::from_slice(payload).unwrap();
            let challenge: [u8; 32] = URL_SAFE_NO_PAD
                .decode(value["challenge"].as_str().unwrap())
                .unwrap()
                .try_into()
                .unwrap();
            let mut signature = key.sign(&auth::preimage(&public, &challenge)).to_bytes();
            if !valid {
                signature[0] ^= 1;
            }
            socket
                .send(Message::Binary(
                    p::json(
                        p::AUTHENTICATE,
                        &serde_json::json!({"signature":URL_SAFE_NO_PAD.encode(signature)}),
                    )
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();
            let Message::Binary(raw) = socket.next().await.unwrap().unwrap() else {
                panic!("binary result required");
            };
            assert_eq!(
                p::decode(&raw).unwrap().0,
                if valid { p::READY } else { p::FAILURE }
            );
            assert_eq!(broker.lookup(&auth::derive(&public)).is_some(), valid);
            if valid {
                socket
                    .send(Message::Ping(Bytes::from_static(b"alive")))
                    .await
                    .unwrap();
                assert_eq!(
                    socket.next().await.unwrap().unwrap(),
                    Message::Pong(Bytes::from_static(b"alive"))
                );
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            } else {
                task.await.unwrap();
            }
            assert!(broker.lookup(&auth::derive(&public)).is_none());
        }
    }
    #[test]
    fn websocket_handshake_preserves_path_origin_and_key_validation() {
        let mut headers = HeaderMap::new();
        headers.insert("sec-websocket-version", "13".parse().unwrap());
        headers.insert(
            "sec-websocket-key",
            "dGhlIHNhbXBsZSBub25jZQ==".parse().unwrap(),
        );
        let mut request = Request::new(
            Method::GET,
            "https://example.test/".parse().unwrap(),
            headers,
            Body::empty(),
        );
        let response = handshake(&request);
        assert_eq!(response.status, StatusCode::SWITCHING_PROTOCOLS);
        assert_eq!(
            response.headers["sec-websocket-accept"],
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
        request
            .headers
            .insert("origin", "https://example.test".parse().unwrap());
        assert_eq!(handshake(&request).status, StatusCode::FORBIDDEN);
        request.headers.remove("origin");
        request.uri = "/missing".parse().unwrap();
        assert_eq!(handshake(&request).status, StatusCode::NOT_FOUND);
        request.uri = "/".parse().unwrap();
        request
            .headers
            .insert("sec-websocket-key", "bad".parse().unwrap());
        assert_eq!(handshake(&request).status, StatusCode::BAD_REQUEST);
    }
}
