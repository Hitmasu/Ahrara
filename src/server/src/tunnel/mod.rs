//! Optional named Cloudflare Tunnel, serving the broker inside the Rust process.
//!
//! The edge client is derived from [libcfd](https://github.com/apersomany/libcfd)
//! 0.2.0 by apersomany, under the Apache-2.0 license in `LICENSE`.
mod discovery;
mod edge;
mod io;

use crate::{
    broker::{Broker, Connection},
    limits::ClientPermit,
    protocol as p,
};
use anyhow::{Context, Result, anyhow, ensure};
use axum::http::{Method, StatusCode, request::Parts};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::FutureExt;
use std::{env, sync::Arc};
use tokio::task::JoinSet;
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        handshake::derive_accept_key,
        protocol::{Role, WebSocketConfig},
    },
};
use tokio_util::{
    compat::{Compat, FuturesAsyncReadCompatExt},
    sync::CancellationToken,
};

pub type Socket = WebSocketStream<Compat<io::H2Stream>>;

/// A named Tunnel's registration identity.
struct Tunnel {
    account_tag: String,
    secret: Vec<u8>,
    id: [u8; 16],
    /// Region override carried by the token.
    endpoint: Option<String>,
}

impl Tunnel {
    /// Parses a dashboard connector token (`cloudflared tunnel run --token`):
    /// base64 JSON with the account tag `a`, base64 secret `s`, Tunnel ID `t`
    /// and optional endpoint `e`.
    fn from_token(token: &str) -> Result<Self> {
        #[derive(serde::Deserialize)]
        struct Token {
            a: String,
            s: String,
            t: String,
            #[serde(default)]
            e: Option<String>,
        }

        let token: Token = serde_json::from_slice(&STANDARD.decode(token)?)?;
        let secret = STANDARD.decode(&token.s)?;
        ensure!(
            !token.a.is_empty() && !secret.is_empty(),
            "incomplete token"
        );
        let uuid = token.t.as_bytes();
        ensure!(
            uuid.len() == 36 && [8, 13, 18, 23].iter().all(|&dash| uuid[dash] == b'-'),
            "invalid Tunnel ID"
        );
        let mut id = [0; 16];
        hex::decode_to_slice(token.t.replace('-', ""), &mut id)?;

        Ok(Self {
            account_tag: token.a,
            secret,
            id,
            endpoint: token.e,
        })
    }
}

pub struct Configuration {
    tunnel: Arc<Tunnel>,
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
    let tunnel = Tunnel::from_token(token.trim()).map_err(|_| anyhow!("invalid Tunnel token"))?;
    let connections = crate::setting("AHRARA_TUNNEL_CONNECTIONS", "4").parse::<u8>()?;
    ensure!(
        (1..=4).contains(&connections),
        "Tunnel connection count must be 1..4"
    );

    Ok(Some(Configuration {
        tunnel: Arc::new(tunnel),
        connections,
    }))
}

/// Validates a WebSocket upgrade from the edge and returns its accept key.
fn handshake(request: &Parts) -> Result<String, StatusCode> {
    if request.uri.path() != "/" {
        return Err(StatusCode::NOT_FOUND);
    }
    if request
        .headers
        .get("origin")
        .is_some_and(|value| !value.is_empty())
    {
        return Err(StatusCode::FORBIDDEN);
    }
    let key = request
        .headers
        .get("sec-websocket-key")
        .filter(|key| STANDARD.decode(key).is_ok_and(|key| key.len() == 16));
    match key {
        Some(key)
            if request.method == Method::GET
                && request
                    .headers
                    .get("sec-websocket-version")
                    .is_some_and(|version| version == "13") =>
        {
            Ok(derive_accept_key(key.as_bytes()))
        }
        _ => Err(StatusCode::BAD_REQUEST),
    }
}

/// Hands the edge stream to the broker without proxy buffers.
fn serve_stream(
    broker: Arc<Broker>,
    stream: io::H2Stream,
    client: ClientPermit,
) -> impl Future<Output = ()> {
    let config = WebSocketConfig::default()
        .read_buffer_size(crate::WEBSOCKET_READ_BUFFER)
        .write_buffer_size(0)
        .max_write_buffer_size(p::MAX_FRAME * 2)
        .max_message_size(Some(p::MAX_CONTROL + 6 + crate::secure::TAG_LEN))
        .max_frame_size(Some(p::MAX_CONTROL + 6 + crate::secure::TAG_LEN));
    // Wrapping a raw stream completes on its first poll; building it here keeps
    // that future out of the long-lived session task.
    let socket = WebSocketStream::from_raw_socket(stream.compat(), Role::Server, Some(config))
        .now_or_never()
        .expect("a raw WebSocket needs no handshake");
    broker.serve_connection(
        Connection::Tunnel(Box::new(socket)),
        CancellationToken::new(),
        client,
    )
}

pub async fn serve(config: Configuration, broker: Arc<Broker>) -> Result<()> {
    let mut tasks = JoinSet::new();
    for index in 0..config.connections {
        tasks.spawn(edge::run(config.tunnel.clone(), index, broker.clone()));
    }

    crate::event(
        "tunnel_started",
        serde_json::json!({"connections": config.connections, "transport": "http2"}),
    );

    // Retryable failures stay inside the connector; a permanent registration failure stops the broker.
    while let Some(result) = tasks.join_next().await {
        result?.map_err(|_| anyhow!("Tunnel connector stopped"))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;

    #[test]
    fn websocket_handshake_preserves_path_origin_and_key_validation() {
        let request = |uri: &str, key: &str, origin: Option<&str>| {
            let mut request = Request::get(uri)
                .header("sec-websocket-version", "13")
                .header("sec-websocket-key", key);
            if let Some(origin) = origin {
                request = request.header("origin", origin);
            }
            request.body(()).unwrap().into_parts().0
        };
        let key = "dGhlIHNhbXBsZSBub25jZQ=="; // gitleaks:allow: RFC 6455 example nonce.

        assert_eq!(
            handshake(&request("/", key, None)).unwrap(),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
        assert_eq!(
            handshake(&request("/", key, Some("https://example.test"))),
            Err(StatusCode::FORBIDDEN)
        );
        assert_eq!(
            handshake(&request("/missing", key, None)),
            Err(StatusCode::NOT_FOUND)
        );
        assert_eq!(
            handshake(&request("/", "bad", None)),
            Err(StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn token_parsing_requires_a_complete_identity() {
        let token = |json: &str| STANDARD.encode(json);
        let tunnel = Tunnel::from_token(&token(
            r#"{"a":"abc123","s":"dG9wLXNlY3JldA==","t":"550e8400-e29b-41d4-a716-446655440000","e":"us"}"#,
        ))
        .unwrap();

        assert_eq!(tunnel.account_tag, "abc123");
        assert_eq!(tunnel.secret, b"top-secret");
        assert_eq!(hex::encode(tunnel.id), "550e8400e29b41d4a716446655440000");
        assert_eq!(tunnel.endpoint.as_deref(), Some("us"));
        for invalid in [
            r#"{"a":"","s":"dG9wLXNlY3JldA==","t":"550e8400-e29b-41d4-a716-446655440000"}"#,
            r#"{"a":"abc123","s":"","t":"550e8400-e29b-41d4-a716-446655440000"}"#,
            r#"{"a":"abc123","s":"dG9wLXNlY3JldA==","t":"550e8400e29b41d4a716446655440000"}"#,
            r#"{"a":"abc123","s":"dG9wLXNlY3JldA==","t":"550e8400-e29b-41d4-a716-44665544000z"}"#,
        ] {
            assert!(Tunnel::from_token(&token(invalid)).is_err());
        }
        assert!(Tunnel::from_token("not base64").is_err());
    }
}
