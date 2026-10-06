use crate::{
    limits::ClientPermit,
    protocol::{self as p, Route},
    secure,
};
use anyhow::{Context, Result, bail, ensure};
use axum::{
    body::Bytes,
    extract::ws::{Message, WebSocket},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, VerifyingKey};
use futures_util::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, hash_map::Entry},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    sync::{Mutex as AsyncMutex, oneshot},
    time::timeout,
};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

enum WireSink {
    WebSocket(SplitSink<WebSocket, Message>),
    #[cfg(test)]
    Channel(tokio::sync::mpsc::Sender<Bytes>),
    #[cfg(feature = "tunnel")]
    Tunnel(SplitSink<crate::tunnel::Socket, tokio_tungstenite::tungstenite::Message>),
}

impl WireSink {
    async fn send(&mut self, message: Message) -> Result<()> {
        match self {
            Self::WebSocket(sink) => sink.send(message).await?,
            #[cfg(feature = "tunnel")]
            Self::Tunnel(sink) => sink.send(tunnel_message(message)).await?,
            #[cfg(test)]
            Self::Channel(sender) => match message {
                Message::Binary(frame) => sender.send(frame).await?,
                Message::Pong(_) => {}
                _ => bail!("unsupported channel message"),
            },
        }

        Ok(())
    }
}

enum WireReader {
    WebSocket(SplitStream<WebSocket>),
    #[cfg(test)]
    Channel(tokio::sync::mpsc::Receiver<Message>),
    #[cfg(feature = "tunnel")]
    Tunnel(SplitStream<crate::tunnel::Socket>),
}

impl WireReader {
    async fn next(&mut self) -> Option<Result<Message>> {
        match self {
            Self::WebSocket(reader) => reader.next().await.map(|value| value.map_err(Into::into)),
            #[cfg(feature = "tunnel")]
            Self::Tunnel(reader) => reader
                .next()
                .await
                .map(|value| value.map(from_tunnel_message).map_err(Into::into)),
            #[cfg(test)]
            Self::Channel(reader) => reader.recv().await.map(Ok),
        }
    }
}

struct Sink {
    wire: WireSink,
    secure: secure::Sender,
}

impl Sink {
    async fn send(&mut self, message: Message) -> Result<()> {
        // The peer's sink mutex preserves ciphertext nonce order through the socket write.
        self.wire.send(seal(message, &mut self.secure)?).await
    }
}

struct Reader {
    wire: WireReader,
    secure: secure::Receiver,
}

impl Reader {
    async fn next(&mut self) -> Option<Result<Message>> {
        self.wire
            .next()
            .await
            .map(|message| message.and_then(|message| open(message, &mut self.secure)))
    }
}

fn seal(message: Message, sender: &mut secure::Sender) -> Result<Message> {
    match message {
        Message::Binary(frame) => Ok(Message::Binary(sender.seal(&frame)?.into())),
        message => Ok(message),
    }
}

fn open(message: Message, receiver: &mut secure::Receiver) -> Result<Message> {
    match message {
        Message::Binary(frame) => Ok(Message::Binary(receiver.open(&frame)?.into())),
        message => Ok(message),
    }
}

pub enum Connection {
    WebSocket(Box<WebSocket>),
    #[cfg(feature = "tunnel")]
    Tunnel(Box<crate::tunnel::Socket>),
    /// In-memory transport for broker tests.
    #[cfg(test)]
    Channel {
        incoming: tokio::sync::mpsc::Receiver<Message>,
        outgoing: tokio::sync::mpsc::Sender<Bytes>,
    },
}

impl Connection {
    fn split(self) -> (WireSink, WireReader) {
        match self {
            Self::WebSocket(ws) => {
                let (sink, reader) = (*ws).split();
                (WireSink::WebSocket(sink), WireReader::WebSocket(reader))
            }
            #[cfg(test)]
            Self::Channel { incoming, outgoing } => {
                (WireSink::Channel(outgoing), WireReader::Channel(incoming))
            }
            #[cfg(feature = "tunnel")]
            Self::Tunnel(ws) => {
                let (sink, reader) = (*ws).split();
                (WireSink::Tunnel(sink), WireReader::Tunnel(reader))
            }
        }
    }
}

async fn handshake(connection: Connection, key: &[u8; 32]) -> Result<(Sink, Reader, [u8; 32])> {
    let (mut sink, mut reader) = connection.split();
    let mut state = secure::responder(key)?;
    let message = loop {
        match reader.next().await.context("connection closed")?? {
            Message::Binary(message) => break message,
            Message::Ping(payload) => sink.send(Message::Pong(payload)).await?,
            Message::Pong(_) => {}
            _ => bail!("expected secure handshake"),
        }
    };
    ensure!(
        message.len() <= secure::MAX_HANDSHAKE,
        "secure handshake too large"
    );
    let mut buffer = [0; secure::MAX_HANDSHAKE];
    ensure!(
        state.read_message(&message, &mut buffer)? == 0,
        "unexpected secure handshake payload"
    );
    let length = state.write_message(&[], &mut buffer)?;
    sink.send(Message::Binary(buffer[..length].to_vec().into()))
        .await?;
    let binding = state.get_handshake_hash().try_into()?;
    let (sender, receiver) = secure::into_session(state)?;
    Ok((
        Sink {
            wire: sink,
            secure: sender,
        },
        Reader {
            wire: reader,
            secure: receiver,
        },
        binding,
    ))
}

#[cfg(feature = "tunnel")]
fn tunnel_message(message: Message) -> tokio_tungstenite::tungstenite::Message {
    use tokio_tungstenite::tungstenite::Message as Wire;
    match message {
        Message::Binary(bytes) => Wire::Binary(bytes),
        Message::Ping(bytes) => Wire::Ping(bytes),
        Message::Pong(bytes) => Wire::Pong(bytes),
        Message::Text(text) => Wire::Text(text.to_string().into()),
        Message::Close(_) => Wire::Close(None),
    }
}

#[cfg(feature = "tunnel")]
fn from_tunnel_message(message: tokio_tungstenite::tungstenite::Message) -> Message {
    use tokio_tungstenite::tungstenite::Message as Wire;
    match message {
        Wire::Binary(bytes) => Message::Binary(bytes),
        Wire::Ping(bytes) => Message::Ping(bytes),
        Wire::Pong(bytes) => Message::Pong(bytes),
        Wire::Text(text) => Message::Text(text.to_string().into()),
        Wire::Close(_) | Wire::Frame(_) => Message::Close(None),
    }
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub transfer_id: String,
    #[serde(default)]
    pub recipient: String,
    #[serde(default, rename = "code")]
    pub _code: String,
    #[serde(skip)]
    pub kind: u8,
}

#[derive(Default)]
struct Pending {
    reservation: String,
    replies: HashMap<String, oneshot::Sender<Reply>>,
}

pub struct Peer {
    sink: AsyncMutex<Option<Sink>>,
    pending: Mutex<Pending>,
    pub closed: CancellationToken,
    last_ping: AtomicU64,
}

pub struct Broker {
    pub admission: Arc<crate::limits::Admission>,
    peers: RwLock<HashMap<Route, Arc<Peer>>>,
    started: Instant,
    pub stopped: CancellationToken,
    transport_key: Zeroizing<[u8; 32]>,
}

/// The frame was sent, but the peer did not confirm it before the deadline.
#[derive(Debug)]
pub struct ReplyTimeout;

impl std::fmt::Display for ReplyTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("client reply timed out")
    }
}

impl std::error::Error for ReplyTimeout {}

impl Broker {
    pub fn encrypted_with_limits(key: [u8; 32], limits: crate::limits::RuntimeLimits) -> Arc<Self> {
        Arc::new(Self {
            admission: crate::limits::Admission::new(limits),
            peers: RwLock::new(HashMap::new()),
            started: Instant::now(),
            stopped: CancellationToken::new(),
            transport_key: Zeroizing::new(key),
        })
    }

    #[cfg(test)]
    pub fn encrypted(key: [u8; 32]) -> Arc<Self> {
        Self::encrypted_with_limits(key, crate::limits::RuntimeLimits::default())
    }

    pub fn lookup(&self, route: &Route) -> Option<Arc<Peer>> {
        self.peers.read().unwrap().get(route).cloned()
    }

    /// Connects an Identity without a client socket, replacing any current connection.
    #[cfg(test)]
    pub fn attach(&self, route: Route) -> Arc<Peer> {
        let peer = Arc::new(Peer {
            sink: AsyncMutex::new(None),
            pending: Mutex::new(Pending::default()),
            closed: CancellationToken::new(),
            last_ping: AtomicU64::new(0),
        });
        self.peers.write().unwrap().insert(route, peer.clone());
        peer
    }

    pub async fn sweep(self: Arc<Self>) {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            tokio::select! {
                _ = self.stopped.cancelled() => break,
                _ = interval.tick() => {
                    let now = self.started.elapsed().as_secs();
                    self.expire_idle(now);
                    self.admission.sweep();
                }
            }
        }

        for peer in self.peers.read().unwrap().values() {
            peer.closed.cancel();
        }
    }

    fn expire_idle(&self, now: u64) {
        // One global heartbeat sweep avoids a timer for each idle connection.
        for peer in self.peers.read().unwrap().values() {
            if now.saturating_sub(peer.last_ping.load(Ordering::Relaxed)) > 210 {
                peer.closed.cancel();
            }
        }
    }

    pub fn serve(
        self: Arc<Self>,
        ws: WebSocket,
        client: ClientPermit,
    ) -> impl std::future::Future<Output = ()> {
        self.serve_connection(
            Connection::WebSocket(Box::new(ws)),
            CancellationToken::new(),
            client,
        )
    }

    /// Serves one client connection; `client` holds its source's slot until it ends.
    #[allow(clippy::manual_async_fn)]
    pub fn serve_connection(
        self: Arc<Self>,
        connection: Connection,
        closed: CancellationToken,
        client: ClientPermit,
    ) -> impl std::future::Future<Output = ()> {
        // An async fn stores its arguments twice; a block capturing them stores them once.
        async move {
            // Every idle connection stores this future, sized by its largest await. Rare
            // steps are boxed: handshake, authentication, their timers and every send.
            let Some((sink, mut reader, (route, fingerprint))) = Box::pin(async {
                tokio::select! {
                    _ = closed.cancelled() => None,
                    _ = self.stopped.cancelled() => None,
                    result = timeout(Duration::from_secs(15), async {
                        let (mut sink, mut reader, binding) = handshake(connection, &self.transport_key).await?;
                        let identity = authenticate(&mut sink, &mut reader, &binding, client.work).await?;
                        Ok::<_, anyhow::Error>((sink, reader, identity))
                    }) => result.ok().and_then(Result::ok),
                }
            })
            .await
            else {
                return;
            };
            let peer = Arc::new(Peer {
                sink: AsyncMutex::new(Some(sink)),
                pending: Mutex::new(Pending::default()),
                closed,
                last_ping: AtomicU64::new(self.started.elapsed().as_secs()),
            });
            // `entry` copies the route; a borrow would keep it in every idle future.
            let inserted = match self.peers.write().unwrap().entry(route) {
                Entry::Vacant(slot) if !self.stopped.is_cancelled() => {
                    slot.insert(peer.clone());
                    true
                }
                _ => false,
            };
            if !inserted {
                let _ = Box::pin(peer.send(p::FAILURE, &json!({"code": "identity_in_use"}))).await;

                return;
            }

            // Edge loss aborts stream futures: retain cleanup even when this task is dropped.
            struct Identity {
                broker: Arc<Broker>,
                route: Route,
                peer: Arc<Peer>,
            }

            impl Drop for Identity {
                fn drop(&mut self) {
                    self.peer.closed.cancel();
                    let mut peers = self.broker.peers.write().unwrap();
                    if peers
                        .get(&self.route)
                        .is_some_and(|peer| Arc::ptr_eq(peer, &self.peer))
                    {
                        peers.remove(&self.route);
                    }

                    // Never log while holding the lock every login and delivery needs.
                    drop(peers);
                    crate::event("identity_disconnected", json!({}));
                }
            }

            let _identity = Identity {
                broker: self.clone(),
                route,
                peer: peer.clone(),
            };
            crate::event("identity_connected", json!({}));
            if Box::pin(peer.send(
                p::READY,
                &json!({
                    "route_id": hex::encode(route),
                    "fingerprint": hex::encode(fingerprint)
                }),
            ))
            .await
            .is_ok()
            {
                loop {
                    let frame = tokio::select! {
                        _ = peer.closed.cancelled() => break,
                        value = reader.next() => value,
                    };

                    match frame {
                        Some(Ok(Message::Binary(raw))) => {
                            let Ok((kind, payload)) = p::decode(&raw) else {
                                break;
                            };
                            if ![p::ACCEPT, p::REJECT, p::ACK, p::NACK].contains(&kind) {
                                break;
                            }

                            let Ok(mut reply) = serde_json::from_slice::<Reply>(payload) else {
                                break;
                            };
                            reply.kind = kind;
                            let waiter = peer
                                .pending
                                .lock()
                                .unwrap()
                                .replies
                                .remove(&format!("{}:{}", reply.transfer_id, reply.recipient));

                            if let Some(waiter) = waiter {
                                let _ = waiter.send(reply);
                            }
                        }
                        Some(Ok(Message::Ping(payload))) => {
                            peer.last_ping
                                .store(self.started.elapsed().as_secs(), Ordering::Relaxed);
                            // Flush the native PONG even if no email is being written.
                            if !matches!(
                                Box::pin(timeout(
                                    Duration::from_secs(5),
                                    peer.message(Message::Pong(payload))
                                ))
                                .await,
                                Ok(Ok(()))
                            ) {
                                break;
                            }
                        }
                        Some(Ok(Message::Pong(_))) => {}
                        _ => break,
                    }
                }
            }

            peer.closed.cancel();
            Box::pin(peer.sink.lock()).await.take();
            // Moving the whole permit in, not just its `work`, holds the slot until here.
            drop(client);
        }
    }
}

impl Peer {
    pub fn reserve(&self, id: &str) -> bool {
        let mut state = self.pending.lock().unwrap();
        if self.closed.is_cancelled() || (!state.reservation.is_empty() && state.reservation != id)
        {
            return false;
        }

        state.reservation.clear();
        state.reservation.push_str(id);
        true
    }

    pub fn release(&self, id: &str) {
        let mut state = self.pending.lock().unwrap();
        if state.reservation == id {
            state.reservation.clear();
        }
    }

    async fn message(&self, message: Message) -> Result<()> {
        self.sink
            .lock()
            .await
            .as_mut()
            .context("client disconnected")?
            .send(message)
            .await?;

        Ok(())
    }

    pub async fn frame(&self, bytes: Bytes) -> Result<()> {
        tokio::select! {
            _ = self.closed.cancelled() => bail!("client disconnected"),
            result = timeout(Duration::from_secs(30), self.message(Message::Binary(bytes))) => { result??; Ok(()) }
        }
    }

    pub async fn send(&self, kind: u8, value: &impl Serialize) -> Result<()> {
        self.frame(p::json(kind, value)?.into()).await
    }

    pub async fn ask(
        &self,
        kind: u8,
        value: &impl Serialize,
        id: &str,
        recipient: &str,
        seconds: u64,
    ) -> Result<Reply> {
        let key = format!("{id}:{recipient}");
        let (send, receive) = oneshot::channel();
        self.pending
            .lock()
            .unwrap()
            .replies
            .insert(key.clone(), send);

        // Cancellation of an SMTP transaction must also remove its waiter.
        struct Waiter<'a>(&'a Peer, String);

        impl Drop for Waiter<'_> {
            fn drop(&mut self) {
                self.0.pending.lock().unwrap().replies.remove(&self.1);
            }
        }

        let _waiter = Waiter(self, key);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
        tokio::time::timeout_at(deadline, self.send(kind, value)).await??;
        tokio::time::timeout_at(deadline, async {
            tokio::select! {
                _ = self.closed.cancelled() => bail!("client disconnected"),
                reply = receive => Ok(reply.context("client disconnected")?),
            }
        })
        .await
        .map_err(|_| ReplyTimeout)?
    }
}

async fn read<T: DeserializeOwned>(
    sink: &mut Sink,
    reader: &mut Reader,
    expected: u8,
) -> Result<T> {
    loop {
        match reader.next().await.context("disconnected")?? {
            Message::Binary(raw) => {
                let (kind, payload) = p::decode(&raw)?;
                ensure!(kind == expected, "unexpected protocol frame");
                return Ok(serde_json::from_slice(payload)?);
            }
            Message::Ping(payload) => sink.send(Message::Pong(payload)).await?,
            Message::Pong(_) => {}
            _ => bail!("binary frame required"),
        }
    }
}

/// Returns the route and fingerprint of the Identity proven for this secure session.
/// A `work` above zero also requires that much proof of work over the challenge.
async fn authenticate(
    sink: &mut Sink,
    reader: &mut Reader,
    binding: &[u8; 32],
    work: u8,
) -> Result<(Route, [u8; 32])> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Hello {
        public_key: String,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Signed {
        signature: String,
        #[serde(default)]
        work: u64,
    }

    let mut failure_code = Some("invalid_hello");
    let result = async {
        let hello: Hello = read(sink, reader, p::HELLO)
            .await
            .context("invalid_hello")?;
        failure_code = Some("invalid_public_key");
        let public: [u8; 32] = URL_SAFE_NO_PAD
            .decode(hello.public_key)?
            .as_slice()
            .try_into()
            .context("invalid_public_key")?;
        failure_code = Some("authentication_failed");
        let mut challenge = [0; 32];
        OsRng.fill_bytes(&mut challenge);
        let mut request = json!({"challenge": URL_SAFE_NO_PAD.encode(challenge)});
        if work > 0 {
            request["work"] = work.into();
        }

        sink.send(Message::Binary(p::json(p::CHALLENGE, &request)?.into()))
            .await?;
        let signed: Signed = read(sink, reader, p::AUTHENTICATE).await?;
        // Clients without proof of work treat any FAILURE as final; closing lets them retry later.
        failure_code = None;
        ensure!(
            p::work(&challenge, signed.work) >= u32::from(work),
            "proof of work required"
        );
        failure_code = Some("authentication_failed");
        let signature = Signature::from_slice(&URL_SAFE_NO_PAD.decode(signed.signature)?)?;
        let preimage = secure::auth_preimage(&public, &challenge, binding);
        VerifyingKey::from_bytes(&public)?.verify_strict(&preimage, &signature)?;
        Ok((p::derive_route(&public), Sha256::digest(public).into()))
    }
    .await;
    if let (Err(_), Some(code)) = (&result, failure_code) {
        let _ = sink
            .send(Message::Binary(
                p::json(p::FAILURE, &json!({"code": code}))?.into(),
            ))
            .await;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use tokio::sync::mpsc;

    struct Client {
        send: mpsc::Sender<Message>,
        receive: mpsc::Receiver<Bytes>,
        sender: secure::Sender,
        receiver: secure::Receiver,
        binding: [u8; 32],
    }

    impl Client {
        async fn send(&mut self, kind: u8, value: &impl Serialize) {
            let cipher = self.sender.seal(&p::json(kind, value).unwrap()).unwrap();
            self.send
                .send(Message::Binary(cipher.into()))
                .await
                .unwrap();
        }

        async fn receive(&mut self, kind: u8) -> serde_json::Value {
            let raw = timeout(Duration::from_secs(1), self.receive.recv())
                .await
                .unwrap()
                .unwrap();
            let frame = self.receiver.open(&raw).unwrap();
            let (actual, payload) = p::decode(&frame).unwrap();

            assert_eq!(actual, kind);
            serde_json::from_slice(payload).unwrap()
        }

        async fn login(&mut self, key: &SigningKey, binding: [u8; 32], expected: u8) {
            let public = key.verifying_key().to_bytes();
            self.send(
                p::HELLO,
                &json!({"public_key": URL_SAFE_NO_PAD.encode(public)}),
            )
            .await;
            self.authenticate(key, binding, true).await;
            self.receive(expected).await;
        }

        /// Answers the challenge, solving any requested proof of work if `prove`.
        async fn authenticate(&mut self, key: &SigningKey, binding: [u8; 32], prove: bool) {
            let public = key.verifying_key().to_bytes();
            let request = self.receive(p::CHALLENGE).await;
            let challenge: [u8; 32] = URL_SAFE_NO_PAD
                .decode(request["challenge"].as_str().unwrap())
                .unwrap()
                .try_into()
                .unwrap();
            let signature = key.sign(&secure::auth_preimage(&public, &challenge, &binding));
            let mut reply = json!({"signature": URL_SAFE_NO_PAD.encode(signature.to_bytes())});
            if let Some(work) = request["work"].as_u64().filter(|_| prove) {
                reply["work"] = p::solve_work(&challenge, work as u8).into();
            }

            self.send(p::AUTHENTICATE, &reply).await;
        }
    }

    const SOURCE: std::net::IpAddr = std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 9));

    async fn encrypted_client(
        closed: CancellationToken,
    ) -> (Arc<Broker>, Client, tokio::task::JoinHandle<()>) {
        encrypted_client_with_work(closed, 0).await
    }

    async fn encrypted_client_with_work(
        closed: CancellationToken,
        work: u8,
    ) -> (Arc<Broker>, Client, tokio::task::JoinHandle<()>) {
        let key = snow::Builder::new(secure::PATTERN.parse().unwrap())
            .generate_keypair()
            .unwrap();
        let private = Zeroizing::new(key.private);
        let broker = Broker::encrypted_with_limits(
            private.as_slice().try_into().unwrap(),
            crate::limits::RuntimeLimits {
                clients_per_source: 1,
                ..crate::limits::RuntimeLimits::default()
            },
        );
        let (send, incoming) = mpsc::channel(1);
        let (outgoing, mut receive) = mpsc::channel(1);
        let mut client = broker.admission.try_client(Some(SOURCE)).unwrap();
        client.work = work;
        let connection = broker.clone().serve_connection(
            Connection::Channel { incoming, outgoing },
            closed,
            client,
        );
        eprintln!(
            "encrypted broker connection future bytes={}",
            std::mem::size_of_val(&connection)
        );
        let task = tokio::spawn(connection);
        let mut handshake = secure::initiator(key.public.as_slice().try_into().unwrap()).unwrap();
        let mut message = [0; secure::MAX_HANDSHAKE];
        let length = handshake.write_message(&[], &mut message).unwrap();
        send.send(Message::Binary(message[..length].to_vec().into()))
            .await
            .unwrap();
        let response = timeout(Duration::from_secs(1), receive.recv())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(handshake.read_message(&response, &mut message).unwrap(), 0);
        let binding = handshake.get_handshake_hash().try_into().unwrap();
        let (sender, receiver) = secure::into_session(handshake).unwrap();
        (
            broker,
            Client {
                send,
                receive,
                sender,
                receiver,
                binding,
            },
            task,
        )
    }

    #[tokio::test]
    async fn identity_signature_is_bound_to_its_secure_handshake() {
        for valid in [true, false] {
            let (broker, mut client, task) = encrypted_client(CancellationToken::new()).await;
            let key = SigningKey::from_bytes(&[8; 32]);
            let mut binding = client.binding;
            if !valid {
                binding[0] ^= 1;
            }

            client
                .login(&key, binding, if valid { p::READY } else { p::FAILURE })
                .await;

            assert_eq!(
                broker
                    .lookup(&p::derive_route(&key.verifying_key().to_bytes()))
                    .is_some(),
                valid
            );
            drop(client);
            timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap();

            assert!(broker.peers.read().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn encrypted_delivery_requires_authentic_ack_and_rejects_replay() {
        for invalid in ["valid", "tampered", "plaintext", "replayed"] {
            let (broker, mut client, task) = encrypted_client(CancellationToken::new()).await;
            let key = SigningKey::from_bytes(&[8; 32]);
            client.login(&key, client.binding, p::READY).await;
            let peer = broker
                .lookup(&p::derive_route(&key.verifying_key().to_bytes()))
                .unwrap();
            let id = "synthetic-transfer";
            let reply = p::json(p::ACK, &json!({"transfer_id": id})).unwrap();
            let mut cipher = client.sender.seal(&reply).unwrap();

            assert!(!cipher.windows(id.len()).any(|part| part == id.as_bytes()));
            if invalid == "replayed" {
                let ask_peer = peer.clone();
                let ask = tokio::spawn(async move {
                    ask_peer
                        .ask(p::END, &json!({"transfer_id": id}), id, "", 5)
                        .await
                });
                client.receive(p::END).await;
                client
                    .send
                    .send(Message::Binary(cipher.clone().into()))
                    .await
                    .unwrap();

                assert_eq!(
                    timeout(Duration::from_secs(1), ask)
                        .await
                        .unwrap()
                        .unwrap()
                        .unwrap()
                        .kind,
                    p::ACK
                );
            }

            let ask_peer = peer.clone();
            let ask = tokio::spawn(async move {
                ask_peer
                    .ask(p::END, &json!({"transfer_id": id}), id, "", 5)
                    .await
            });
            client.receive(p::END).await;
            if invalid == "tampered" {
                cipher[0] ^= 1;
            }
            if invalid == "plaintext" {
                cipher = reply;
            }

            client
                .send
                .send(Message::Binary(cipher.into()))
                .await
                .unwrap();
            let result = timeout(Duration::from_secs(1), ask).await.unwrap().unwrap();
            if invalid == "valid" {
                assert_eq!(result.unwrap().kind, p::ACK);
            } else {
                assert!(result.is_err(), "unauthenticated ACK completed delivery");
            }

            drop(client);
            timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap();

            assert!(broker.peers.read().unwrap().is_empty());
            assert!(peer.pending.lock().unwrap().replies.is_empty());
        }
    }

    #[tokio::test]
    async fn requested_proof_of_work_gates_login_and_its_absence_closes_without_failure() {
        for prove in [true, false] {
            let (broker, mut client, task) =
                encrypted_client_with_work(CancellationToken::new(), 12).await;
            let key = SigningKey::from_bytes(&[9; 32]);
            client
                .send(
                    p::HELLO,
                    &json!({"public_key": URL_SAFE_NO_PAD.encode(key.verifying_key().to_bytes())}),
                )
                .await;
            client.authenticate(&key, client.binding, prove).await;
            if prove {
                client.receive(p::READY).await;
                drop(client);
            } else {
                // Older clients take any FAILURE as final, so the relay only closes.
                let closed = timeout(Duration::from_secs(1), client.receive.recv()).await;

                assert!(closed.unwrap().is_none());
            }

            timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap();

            assert!(broker.peers.read().unwrap().is_empty());
            assert!(
                broker.admission.try_client(Some(SOURCE)).is_some(),
                "the source's only slot was not released"
            );
        }
    }

    #[tokio::test]
    async fn encrypted_connection_rejects_plaintext_hello_without_exposing_challenge() {
        let broker = Broker::encrypted([7; 32]);
        let (send, incoming) = mpsc::channel(1);
        let (outgoing, mut receive) = mpsc::channel(1);
        let task = tokio::spawn(broker.clone().serve_connection(
            Connection::Channel { incoming, outgoing },
            CancellationToken::new(),
            broker.admission.try_client(None).unwrap(),
        ));
        send.send(Message::Binary(
            p::json(
                p::HELLO,
                &json!({"public_key": URL_SAFE_NO_PAD.encode([8; 32])}),
            )
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
        let response = timeout(Duration::from_secs(1), receive.recv())
            .await
            .expect("invalid secure handshake must close promptly");

        assert!(
            response.is_none(),
            "secure endpoint exposed a plaintext authentication response"
        );
        task.await.unwrap();

        assert!(broker.peers.read().unwrap().is_empty());
    }

    fn secure_pair() -> (secure::Sender, secure::Receiver) {
        let key = snow::Builder::new(secure::PATTERN.parse().unwrap())
            .generate_keypair()
            .unwrap();
        let mut client = secure::initiator(key.public.as_slice().try_into().unwrap()).unwrap();
        let mut server = secure::responder(key.private.as_slice().try_into().unwrap()).unwrap();
        let mut message = [0; secure::MAX_HANDSHAKE];
        let mut payload = [0; secure::MAX_HANDSHAKE];
        let length = client.write_message(&[], &mut message).unwrap();
        server
            .read_message(&message[..length], &mut payload)
            .unwrap();
        let length = server.write_message(&[], &mut message).unwrap();
        client
            .read_message(&message[..length], &mut payload)
            .unwrap();
        (
            secure::into_session(server).unwrap().0,
            secure::into_session(client).unwrap().1,
        )
    }

    fn peer(last_ping: u64) -> Arc<Peer> {
        Arc::new(Peer {
            sink: AsyncMutex::new(None),
            pending: Mutex::new(Pending::default()),
            closed: CancellationToken::new(),
            last_ping: AtomicU64::new(last_ping),
        })
    }

    #[tokio::test(start_paused = true)]
    async fn missing_ack_timeout_is_distinct_from_a_stalled_write_and_removes_waiter() {
        for blocked_write in [false, true] {
            let peer = peer(0);
            let (send, mut receive) = mpsc::channel(1);
            let (sender, mut receiver) = secure_pair();
            *peer.sink.lock().await = Some(Sink {
                wire: WireSink::Channel(send),
                secure: sender,
            });
            if blocked_write {
                peer.send(p::ABORT, &json!({"transfer_id": "previous"}))
                    .await
                    .unwrap();
            }

            let error = peer
                .ask(p::END, &json!({"transfer_id": "test"}), "test", "", 1)
                .await
                .unwrap_err();

            assert_eq!(error.is::<ReplyTimeout>(), !blocked_write);
            let frame = receiver.open(&receive.recv().await.unwrap()).unwrap();

            assert_eq!(
                p::decode(&frame).unwrap().0,
                if blocked_write { p::ABORT } else { p::END }
            );

            assert!(peer.pending.lock().unwrap().replies.is_empty());
        }
    }

    #[test]
    fn reservation_is_exclusive_and_old_cleanup_cannot_release_new_mail() {
        let peer = peer(0);
        assert!(peer.reserve("first"));
        assert!(peer.reserve("first"));
        assert!(!peer.reserve("second"));
        peer.release("second");

        assert!(!peer.reserve("second"));
        peer.release("first");

        assert!(peer.reserve("second"));
        peer.release("first");

        assert!(!peer.reserve("third"));
        peer.closed.cancel();

        assert!(!peer.reserve("second"));
    }

    #[test]
    fn idle_sweep_preserves_clients_with_native_ping() {
        let broker = Broker::encrypted([7; 32]);
        let stale = peer(0);
        let alive = peer(90);
        broker.peers.write().unwrap().insert([1; 16], stale.clone());
        broker.peers.write().unwrap().insert([2; 16], alive.clone());
        broker.expire_idle(210);

        assert!(!stale.closed.is_cancelled());
        broker.expire_idle(240);

        assert!(stale.closed.is_cancelled());
        assert!(!alive.closed.is_cancelled());
    }

    #[tokio::test]
    async fn authentication_requires_signed_challenge_and_removes_disconnected_identity() {
        for (valid_signature, abort) in [(true, false), (false, false), (true, true)] {
            let closed = CancellationToken::new();
            let (broker, mut client, task) = encrypted_client(closed.clone()).await;
            let key = SigningKey::from_bytes(&[7; 32]);
            let public = key.verifying_key().to_bytes();
            let mut binding = client.binding;
            // A signature over another handshake does not authenticate this connection.
            if !valid_signature {
                binding[0] ^= 1;
            }

            client
                .login(
                    &key,
                    binding,
                    if valid_signature {
                        p::READY
                    } else {
                        p::FAILURE
                    },
                )
                .await;

            assert_eq!(
                broker.lookup(&p::derive_route(&public)).is_some(),
                valid_signature
            );
            if abort {
                task.abort();
            } else {
                closed.cancel();
            }

            let result = timeout(Duration::from_secs(1), task).await.unwrap();
            if abort {
                assert!(result.unwrap_err().is_cancelled());
            } else {
                result.unwrap();
            }

            assert!(broker.lookup(&p::derive_route(&public)).is_none());
        }
    }
}
