use crate::{auth::decode_address, core::Core, protocol::*};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{SinkExt, Stream, StreamExt};
use rand::Rng;
use serde::Deserialize;
use serde_json::{Value, json as value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, io::BufReader, pin::Pin, sync::Arc, time::Duration};
use tokio::net::TcpStream;
use tokio::time::Instant;
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, connect_async_tls_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};
use tokio_util::sync::CancellationToken;

enum Wire {
    WebSocket(Box<WebSocketStream<MaybeTlsStream<TcpStream>>>),
    Sse(Sse),
}

struct Socket {
    wire: Wire,
    channel: Option<(crate::secure::Sender, crate::secure::Receiver)>,
}

impl Socket {
    fn new(wire: Wire) -> Self {
        Self {
            wire,
            channel: None,
        }
    }
}

struct Sse {
    client: reqwest::Client,
    url: String,
    token: reqwest::header::HeaderValue,
    frames: Pin<Box<dyn Stream<Item = Result<Vec<u8>>> + Send>>,
}

impl Sse {
    async fn connect(core: &Core) -> Result<Self> {
        let mut tls = tls_config(core)?;
        tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let client = Self::client(tls, Duration::from_secs(120))?;
        let response = tokio::time::timeout(
            Duration::from_secs(10),
            client
                .get(&core.config.server_url)
                .header("accept", "text/event-stream")
                .send(),
        )
        .await??;
        if response.status() != reqwest::StatusCode::OK
            || response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_none_or(|v| v.split(';').next() != Some("text/event-stream"))
        {
            bail!("invalid SSE response")
        }

        let mut token = response
            .headers()
            .get("x-ahrara-session")
            .context("missing SSE session")?
            .clone();
        if URL_SAFE_NO_PAD.decode(token.as_bytes())?.len() != 32 {
            bail!("invalid SSE session")
        }

        token.set_sensitive(true);
        let frames = Self::frames(response);
        Ok(Self {
            client,
            url: core.config.server_url.clone(),
            token,
            frames,
        })
    }

    fn client(tls: rustls::ClientConfig, read_timeout: Duration) -> Result<reqwest::Client> {
        Ok(reqwest::Client::builder()
            .use_preconfigured_tls(tls)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            // POST heartbeats can succeed while the independent GET is stalled.
            // Server comments every 30 seconds keep a healthy downlink active.
            .read_timeout(read_timeout)
            .build()?)
    }

    fn frames(response: reqwest::Response) -> Pin<Box<dyn Stream<Item = Result<Vec<u8>>> + Send>> {
        Box::pin(async_stream::try_stream! {
            let mut body = response.bytes_stream();
            let mut parser = SseParser::default();
            while let Some(chunk) = body.next().await {
                for byte in chunk? {
                    if let Some(frame) = parser.push(byte)? { yield frame; }
                }
            }
        })
    }

    async fn post(&mut self, frame: Vec<u8>) -> Result<()> {
        let response = self
            .client
            .post(&self.url)
            .header("x-ahrara-session", self.token.clone())
            .header("content-type", "application/octet-stream")
            .body(frame)
            .send()
            .await?;
        if response.status() != reqwest::StatusCode::NO_CONTENT {
            bail!("SSE POST failed: {}", response.status())
        }

        Ok(())
    }
}

// The Ahrara endpoint emits one bounded data line per frame. Reject malformed
// events instead of buffering arbitrary SSE fields or an unbounded response.
#[derive(Default)]
struct SseParser {
    line: Vec<u8>,
    frame_event: bool,
    frame: Option<Vec<u8>>,
    event_bytes: usize,
}

impl SseParser {
    fn push(&mut self, byte: u8) -> Result<Option<Vec<u8>>> {
        const MAX_EVENT: usize = MAX_FRAME.div_ceil(3) * 4 + 128;
        self.event_bytes += 1;
        if self.event_bytes > MAX_EVENT {
            bail!("SSE event exceeds frame limit")
        }
        if byte != b'\n' {
            self.line.push(byte);
            return Ok(None);
        }

        let mut line = std::mem::take(&mut self.line);
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.is_empty() {
            self.event_bytes = 0;
            if self.frame.is_some() && !self.frame_event {
                bail!("invalid SSE event")
            }

            self.frame_event = false;
            return Ok(self.frame.take());
        }
        if line.starts_with(b":") {
            return Ok(None);
        }
        if line == b"event: frame" {
            if self.frame_event {
                bail!("duplicate SSE event type")
            }

            self.frame_event = true;
        } else if let Some(data) = line.strip_prefix(b"data: ") {
            if self.frame.is_some() {
                bail!("duplicate SSE frame data")
            }

            let frame = URL_SAFE_NO_PAD.decode(data)?;
            if frame.len() > MAX_FRAME {
                bail!("SSE frame exceeds limit")
            }

            self.frame = Some(frame);
        } else {
            bail!("invalid SSE field")
        }

        Ok(None)
    }
}

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
            Ok(wire) => {
                let mut socket = Socket::new(wire);
                tokio::select! {
                    _ = cancel.cancelled() => {
                        // Keep ownership until the relay closes its side. Dropping
                        // TCP alone races immediate reuse of this identity.
                        if let Wire::WebSocket(ws) = &mut socket.wire {
                            let _ = tokio::time::timeout(Duration::from_secs(5), async {
                                ws.as_mut().close(None).await?;
                                while let Some(frame) = ws.next().await {
                                    frame?;
                                }
                                Ok::<(), tokio_tungstenite::tungstenite::Error>(())
                            }).await;
                        }
                        break;
                    }
                    result = connect(&core, &mut socket) => result,
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

async fn send(socket: &mut Socket, kind: u8, value: &Value) -> Result<()> {
    let frame = json(kind, value)?;
    let frame = match &mut socket.channel {
        Some((sender, _)) => sender.seal(&frame)?,
        None => frame,
    };

    send_packet(&mut socket.wire, frame).await
}

async fn send_packet(wire: &mut Wire, frame: Vec<u8>) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(30), async {
        match wire {
            Wire::WebSocket(ws) => Ok(ws.send(Message::Binary(frame.into())).await?),
            Wire::Sse(sse) => sse.post(frame).await,
        }
    })
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

async fn receive(socket: &mut Socket, heartbeat: &mut Heartbeat) -> Result<Vec<u8>> {
    let packet = receive_packet(&mut socket.wire, heartbeat).await?;
    match &mut socket.channel {
        Some((_, receiver)) => receiver.open(&packet),
        None => Ok(packet),
    }
}

async fn receive_packet(wire: &mut Wire, heartbeat: &mut Heartbeat) -> Result<Vec<u8>> {
    if let Wire::Sse(sse) = wire {
        loop {
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(heartbeat.next) => {
                    tokio::time::timeout(heartbeat.timeout, sse.post(Vec::new())).await??;
                    heartbeat.next = Instant::now() + Heartbeat::delay(heartbeat.interval);
                }
                frame = sse.frames.next() => return frame.context("connection closed")?,
            }
        }
    }

    let Wire::WebSocket(socket) = wire else {
        unreachable!()
    };

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
        let file = std::fs::File::open(path).context("cannot open configured TLS CA")?;
        let certs = rustls_pemfile::certs(&mut BufReader::new(file))
            .collect::<std::io::Result<Vec<_>>>()?;
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Offer {
    transfer_id: String,
    recipient: String,
    alias_id: String,
    envelope_sender: String,
    declared_size: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    transfer_id: String,
    recipients: Vec<String>,
    envelope_sender: String,
    declared_size: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct End {
    transfer_id: String,
    total_bytes: usize,
    chunks: u32,
    sha256: String,
}

struct Delivery {
    id: String,
    id_bytes: [u8; 16],
    accepted: BTreeSet<String>,
    recipients: Vec<String>,
    sender: String,
    started: bool,
    raw: Vec<u8>,
    digest: Sha256,
    sequence: u32,
}

impl Delivery {
    fn new(offer: &Offer) -> Result<Self> {
        let id_bytes: [u8; 16] = hex::decode(&offer.transfer_id)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid transfer id"))?;

        Ok(Self {
            id: offer.transfer_id.clone(),
            id_bytes,
            accepted: BTreeSet::new(),
            recipients: Vec::new(),
            sender: offer.envelope_sender.clone(),
            started: false,
            raw: Vec::new(),
            digest: Sha256::new(),
            sequence: 0,
        })
    }

    fn chunk(&mut self, payload: &[u8]) -> Result<()> {
        if !self.started
            || payload.len() <= 20
            || payload[..16] != self.id_bytes
            || u32::from_be_bytes(payload[16..20].try_into()?) != self.sequence
        {
            bail!("invalid chunk sequence")
        }

        let bytes = &payload[20..];
        if self.raw.len() + bytes.len() > MAX_EMAIL {
            bail!("message exceeds 10 MiB")
        }

        self.digest.update(bytes);
        self.raw.extend_from_slice(bytes);
        self.sequence += 1;
        Ok(())
    }

    fn validate_end(&self, end: &End) -> Result<()> {
        if !self.started
            || end.transfer_id != self.id
            || end.total_bytes != self.raw.len()
            || end.chunks != self.sequence
            || end.sha256 != hex::encode(self.digest.clone().finalize())
        {
            bail!("invalid delivery checksum or length")
        }

        Ok(())
    }
}

async fn open_wire(core: &Core) -> Result<Wire> {
    if core.config.server_url.starts_with("https://") {
        Ok(Wire::Sse(Sse::connect(core).await?))
    } else {
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_FRAME))
            .max_frame_size(Some(MAX_FRAME))
            .write_buffer_size(0)
            .max_write_buffer_size(MAX_FRAME * 2);
        let connector = Connector::Rustls(Arc::new(tls_config(core)?));
        let (socket, _) = tokio::time::timeout(
            Duration::from_secs(10),
            connect_async_tls_with_config(
                &core.config.server_url,
                Some(config),
                true,
                Some(connector),
            ),
        )
        .await??;

        Ok(Wire::WebSocket(Box::new(socket)))
    }
}

async fn connect(core: &Arc<Core>, socket: &mut Socket) -> Result<()> {
    let pin = crate::secure::parse_public_key(
        core.config
            .server_public_key
            .as_deref()
            .context("server_public_key is required for encrypted transport")?,
    )?;
    let mut heartbeat = Heartbeat::new(Duration::from_secs(90), Duration::from_secs(30));
    tokio::time::timeout(Duration::from_secs(15), async {
        let binding = {
            let mut handshake = crate::secure::initiator(&pin)?;
            let mut message = [0; crate::secure::MAX_HANDSHAKE];
            let length = handshake.write_message(&[], &mut message)?;
            send_packet(&mut socket.wire, message[..length].to_vec()).await?;
            let response = receive_packet(&mut socket.wire, &mut heartbeat).await?;
            if response.len() > crate::secure::MAX_HANDSHAKE
                || handshake.read_message(&response, &mut message)? != 0
                || !handshake.is_handshake_finished()
            {
                bail!("invalid encrypted transport handshake")
            }

            let binding: [u8; 32] = handshake.get_handshake_hash().try_into()?;
            socket.channel = Some(crate::secure::into_session(handshake)?);
            binding
        };
        send(
            socket,
            HELLO,
            &value!({"public_key":URL_SAFE_NO_PAD.encode(core.identity.public_key())}),
        )
        .await?;
        let frame = receive(socket, &mut heartbeat).await?;
        let (kind, payload) = decode(&frame)?;
        if kind != CHALLENGE {
            bail!("authentication_failed")
        }

        let challenge: Value = serde_json::from_slice(payload)?;
        let challenge = URL_SAFE_NO_PAD.decode(
            challenge["challenge"]
                .as_str()
                .context("missing challenge")?,
        )?;
        let signature = core.identity.sign_bound(&challenge, &binding)?;
        send(
            socket,
            AUTHENTICATE,
            &value!({"signature":URL_SAFE_NO_PAD.encode(signature)}),
        )
        .await?;
        let frame = receive(socket, &mut heartbeat).await?;
        let (kind, payload) = decode(&frame)?;
        let response: Value = serde_json::from_slice(payload)?;
        if kind == FAILURE {
            bail!(
                "{}",
                if response["code"] == "identity_in_use" {
                    "identity_in_use"
                } else {
                    "authentication_failed"
                }
            )
        }
        if kind != READY
            || response["route_id"] != hex::encode(core.identity.route())
            || response["fingerprint"] != core.identity.fingerprint()
        {
            bail!("authentication_failed")
        }

        Ok::<(), anyhow::Error>(())
    })
    .await??;
    core.set_state("connected", None).await;
    let mut delivery: Option<Delivery> = None;

    loop {
        let frame = receive(socket, &mut heartbeat).await?;
        let (kind, payload) = decode(&frame)?;
        match kind {
            OFFER => {
                let offer: Offer = serde_json::from_slice(payload)?;
                let valid = match decode_address(&offer.recipient) {
                    Ok((route, alias, canonical)) => {
                        route == core.identity.route()
                            && canonical == offer.recipient
                            && hex::encode(alias) == offer.alias_id
                    }
                    Err(_) => false,
                };
                let available = delivery.as_ref().is_none_or(|d| {
                    d.id == offer.transfer_id
                        && !d.started
                        && d.accepted.len() < 100
                        && d.sender == offer.envelope_sender
                });
                let accept = valid
                    && available
                    && offer.declared_size.is_none_or(|n| n <= MAX_EMAIL as u64)
                    && core
                        .storage
                        .accepts(core.identity.fingerprint(), offer.recipient.clone())
                        .await?;
                if accept {
                    if delivery.is_none() {
                        delivery = Some(Delivery::new(&offer)?);
                    }

                    delivery
                        .as_mut()
                        .expect("delivery exists")
                        .accepted
                        .insert(offer.recipient.clone());
                }

                send(
                    socket,
                    if accept { ACCEPT } else { REJECT },
                    &value!({"transfer_id":offer.transfer_id,"recipient":offer.recipient}),
                )
                .await?;
            }
            START => {
                let start: Start = serde_json::from_slice(payload)?;
                let d = delivery.as_mut().context("start without accepted offer")?;
                let unique: BTreeSet<_> = start.recipients.iter().cloned().collect();
                if d.started
                    || start.transfer_id != d.id
                    || start.envelope_sender != d.sender
                    || unique.is_empty()
                    || unique.len() != start.recipients.len()
                    || !unique.is_subset(&d.accepted)
                    || start.declared_size.is_some_and(|n| n > MAX_EMAIL as u64)
                {
                    bail!("invalid delivery start")
                }

                d.started = true;
                d.recipients = start.recipients;
                // Aliases share this buffer; reserve only the bounded declared size.
                d.raw = Vec::with_capacity(start.declared_size.unwrap_or(0) as usize);
            }
            CHUNK => delivery
                .as_mut()
                .context("chunk without transfer")?
                .chunk(payload)?,
            END => {
                let end: End = serde_json::from_slice(payload)?;
                let d = delivery.take().context("end without transfer")?;
                d.validate_end(&end)?;
                let saved = core
                    .store_delivery(d.id.clone(), d.recipients, d.sender, d.raw)
                    .await;

                match saved {
                    Ok(emails) => {
                        core.received(emails);
                        send(socket, ACK, &value!({"transfer_id":d.id})).await?;
                    }
                    Err(_) => {
                        send(
                            socket,
                            NACK,
                            &value!({"transfer_id":d.id,"code":"persistence_failed"}),
                        )
                        .await?
                    }
                }
            }
            ABORT => {
                let v: Value = serde_json::from_slice(payload)?;
                if delivery.as_ref().is_some_and(|d| v["transfer_id"] == d.id) {
                    delivery = None;
                }
            }
            _ => bail!("unexpected protocol frame"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn sse_parser_bounds_and_validates_frames() {
        let frame = json(READY, &value!({"route_id":"test"})).unwrap();
        let event = format!(
            "event: frame\r\ndata: {}\r\n\r\n",
            URL_SAFE_NO_PAD.encode(&frame)
        );
        let mut parser = SseParser::default();
        let mut received = Vec::new();

        for byte in event.bytes().chain(event.bytes()) {
            if let Some(frame) = parser.push(byte).unwrap() {
                received.push(frame);
            }
        }

        assert_eq!(received, [frame.clone(), frame]);

        for malformed in [
            "data: YQ\n\n",
            "event: other\n",
            "event: frame\ndata: %%%\n",
            "data: AQIAAAAA\n\n",
        ] {
            let mut parser = SseParser::default();
            assert!(
                malformed.bytes().any(|b| parser.push(b).is_err()),
                "{malformed}"
            );
        }

        let mut parser = SseParser::default();

        assert!((0..MAX_FRAME * 2).any(|_| parser.push(b'x').is_err()));
    }

    #[tokio::test]
    async fn sse_detects_stalled_downlink_while_post_heartbeats_succeed() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let posts = Arc::new(AtomicUsize::new(0));
        let count = posts.clone();
        let app = axum::Router::new().route(
            "/sse",
            axum::routing::get(|| async {
                let first = futures_util::stream::once(async {
                    Ok::<_, std::convert::Infallible>(
                        axum::response::sse::Event::default().comment("connected"),
                    )
                });

                axum::response::Sse::new(first.chain(futures_util::stream::pending()))
            })
            .post(move || {
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::Relaxed);
                    axum::http::StatusCode::NO_CONTENT
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/sse", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
        let client = Sse::client(tls, Duration::from_millis(80)).unwrap();
        let response = client.get(&url).send().await.unwrap();
        let mut socket = Socket::new(Wire::Sse(Sse {
            client,
            url,
            token: reqwest::header::HeaderValue::from_static("test"),
            frames: Sse::frames(response),
        }));
        let mut heartbeat = Heartbeat::new(Duration::from_millis(15), Duration::from_millis(40));
        let result = tokio::time::timeout(
            Duration::from_millis(500),
            receive(&mut socket, &mut heartbeat),
        )
        .await;
        server.abort();

        assert!(
            posts.load(Ordering::Relaxed) >= 2,
            "upload must remain healthy"
        );
        let error = result
            .expect("GET stalled indefinitely while POSTs succeeded")
            .unwrap_err();

        assert!(
            error
                .downcast_ref::<reqwest::Error>()
                .is_some_and(reqwest::Error::is_timeout),
            "{error:#}"
        );
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
            let mut socket = Socket::new(Wire::WebSocket(Box::new(socket)));
            let mut heartbeat =
                Heartbeat::new(Duration::from_millis(30), Duration::from_millis(60));
            let error =
                tokio::time::timeout(Duration::from_secs(2), receive(&mut socket, &mut heartbeat))
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
        let mut socket = Socket::new(Wire::WebSocket(Box::new(socket)));
        let mut heartbeat = Heartbeat::new(Duration::from_secs(90), Duration::from_secs(30));
        let received = tokio::time::timeout(
            Duration::from_secs(110),
            receive(&mut socket, &mut heartbeat),
        )
        .await;

        assert_eq!(
            received.expect("client never initiated heartbeat").unwrap(),
            vec![1, 2, 3]
        );

        peer.await.unwrap();
    }

    #[test]
    fn transfer_requires_order_length_and_checksum_before_commit() {
        let offer = Offer {
            transfer_id: hex::encode([1; 16]),
            recipient: String::new(),
            alias_id: String::new(),
            envelope_sender: String::new(),
            declared_size: None,
        };
        let mut delivery = Delivery::new(&offer).unwrap();
        let mut chunk = vec![1; 16];
        chunk.extend(0u32.to_be_bytes());
        chunk.extend(b"body\r\n");

        assert!(delivery.chunk(&chunk).is_err());
        delivery.started = true;
        delivery.chunk(&chunk).unwrap();

        assert!(delivery.chunk(&chunk).is_err());
        let mut end = End {
            transfer_id: offer.transfer_id,
            total_bytes: 6,
            chunks: 1,
            sha256: hex::encode(Sha256::digest(b"body\r\n")),
        };

        assert!(delivery.validate_end(&end).is_ok());
        end.sha256 = "wrong".into();

        assert!(delivery.validate_end(&end).is_err());
        end.sha256 = hex::encode(Sha256::digest(b"body\r\n"));
        end.total_bytes = 5;

        assert!(delivery.validate_end(&end).is_err());
        chunk[16..20].copy_from_slice(&1u32.to_be_bytes());
        delivery.raw.resize(MAX_EMAIL, 0);

        assert!(delivery.chunk(&chunk).is_err());
    }
}
