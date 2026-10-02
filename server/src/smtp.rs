use crate::{
    auth::{self, Route},
    broker::{Broker, Peer},
    data::Decoder,
    protocol as p, routing,
};
use anyhow::{Result, bail};
use bytes::Bytes;
use futures_util::future::join_all;
use ipnet::IpNet;
use rand::{RngCore, rngs::OsRng};
use serde_json::json;
use sha2::{Digest, Sha256};
use smtp_proto::{MAIL_BODY_7BIT, MAIL_BODY_8BITMIME, Request};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    net::TcpStream,
    time::{Instant, timeout, timeout_at},
};
use tokio_rustls::TlsAcceptor;
use zeroize::{Zeroize, Zeroizing};

trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}
type Socket = BufReader<Box<dyn Stream>>;

// The envelope and transfer metadata also occupy the authenticated Noise record.
const SEND_CHUNK_SIZE: usize = crate::secure::MAX_PLAINTEXT - 26;

#[derive(Clone, Default)]
struct ChunkBuffer(Arc<Zeroizing<Vec<u8>>>);

impl AsRef<[u8]> for ChunkBuffer {
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl ChunkBuffer {
    fn writable(&mut self) -> &mut [u8] {
        // A cancelled or buffered send may still own a frame. Never overwrite it.
        if Arc::get_mut(&mut self.0).is_none() {
            *self = Self::default();
        }
        let bytes = Arc::get_mut(&mut self.0).unwrap();
        bytes.resize(p::MAX_FRAME, 0);
        bytes.as_mut_slice()
    }

    fn payload(&mut self) -> &mut [u8] {
        &mut self.writable()[26..26 + SEND_CHUNK_SIZE]
    }

    fn frame(&mut self, transfer: [u8; 16], sequence: u32, count: usize) -> Bytes {
        let bytes = self.writable();
        bytes[..2].copy_from_slice(&[1, p::CHUNK]);
        bytes[2..6].copy_from_slice(&((20 + count) as u32).to_be_bytes());
        bytes[6..22].copy_from_slice(&transfer);
        bytes[22..26].copy_from_slice(&sequence.to_be_bytes());
        Bytes::from_owner(self.clone()).slice(..26 + count)
    }

    fn clear(&mut self) {
        if let Some(bytes) = Arc::get_mut(&mut self.0) {
            bytes.as_mut_slice().zeroize();
        } else {
            // The last outstanding frame erases its own allocation on drop.
            *self = Self::default();
        }
    }
}

struct Transfer {
    bytes: [u8; 16],
    id: String,
    sender: String,
    size: Option<usize>,
    deadline: Instant,
    peers: HashMap<Route, (Arc<Peer>, Vec<String>)>,
    recipients: HashSet<String>,
    offered: HashSet<String>,
    envelopes: HashSet<String>,
}

impl Transfer {
    fn new(sender: String, size: usize, transaction_timeout: Duration) -> Self {
        let mut bytes = [0; 16];
        OsRng.fill_bytes(&mut bytes);
        Self {
            bytes,
            id: hex::encode(bytes),
            sender,
            size: if size > 0 { Some(size) } else { None },
            deadline: Instant::now() + transaction_timeout,
            peers: HashMap::new(),
            recipients: HashSet::new(),
            offered: HashSet::new(),
            envelopes: HashSet::new(),
        }
    }
    async fn offer(
        &mut self,
        broker: &Broker,
        address: &str,
    ) -> std::result::Result<(), &'static str> {
        let (route, alias, recipient) =
            auth::address(address).map_err(|_| "550 5.1.1 Mailbox unavailable")?;
        if self.recipients.contains(&recipient) {
            return Ok(());
        }
        if self.offered.contains(&recipient) {
            return Err("451 4.4.2 Start a new transaction to retry this recipient");
        }
        if self.offered.len() >= 100 {
            return Err("452 4.5.3 Too many distinct recipients");
        }
        if !broker.admission.allows_route(&route) {
            return Err("450 4.2.1 Delivery confirmation cooldown, retry later");
        }
        let peer = broker
            .lookup(&route)
            .ok_or("550 5.1.1 Mailbox unavailable")?;
        if !peer.reserve(&self.id) {
            return Err("450 4.2.1 Identity busy");
        }
        self.offered.insert(recipient.clone());
        // Record the reservation before awaiting, so cancellation releases it.
        self.peers
            .entry(route)
            .or_insert_with(|| (peer.clone(), Vec::new()));
        let reply=peer.ask(p::OFFER,&json!({"transfer_id":self.id,"recipient":recipient,"alias_id":hex::encode(alias),"envelope_sender":self.sender,"declared_size":self.size}),&self.id,&recipient,broker.admission.settings().offer_timeout.as_secs()).await;
        if let Ok(reply) = &reply
            && reply.kind == p::ACCEPT
        {
            self.peers
                .get_mut(&route)
                .unwrap()
                .1
                .push(recipient.clone());
            self.recipients.insert(recipient);
            return Ok(());
        }
        if self.peers[&route].1.is_empty() {
            self.peers.remove(&route);
            let _ = timeout(
                Duration::from_secs(1),
                peer.send(p::ABORT, &json!({"transfer_id":self.id})),
            )
            .await;
            peer.release(&self.id);
        }
        Err(if reply.is_err() {
            "451 4.4.2 Delivery offer unavailable"
        } else {
            "550 5.1.1 Mailbox unavailable"
        })
    }

    async fn data(
        &mut self,
        io: &mut Socket,
        broker: &Broker,
        trusted: bool,
        buffer: &mut ChunkBuffer,
    ) -> std::result::Result<(), &'static str> {
        let failure = "451 4.4.2 Local delivery not confirmed";
        let mut decoder = Decoder::bounded();
        let mut prefix = Zeroizing::new(Vec::new());
        if trusted {
            loop {
                let count = decoder
                    .read(io, buffer.payload())
                    .await
                    .map_err(|_| "554 5.6.0 Invalid forwarding headers")?;
                if count == 0 {
                    return Err("554 5.6.0 Invalid forwarding headers");
                }
                prefix.extend_from_slice(&buffer.as_ref()[26..26 + count]);
                let end = prefix
                    .windows(4)
                    .position(|s| s == b"\r\n\r\n")
                    .map(|p| p + 4)
                    .or_else(|| prefix.windows(2).position(|s| s == b"\n\n").map(|p| p + 2));
                if let Some(end) = end {
                    let recipient = routing::recipient(&prefix[..end])
                        .map_err(|_| "554 5.6.0 Invalid forwarding recipient")?;
                    self.offer(broker, &recipient).await?;
                    break;
                }
                if prefix.len() >= routing::MAX_HEADERS {
                    return Err("554 5.6.0 Invalid forwarding headers");
                }
            }
        }
        for (peer, recipients) in self.peers.values() {
            peer.send(p::START,&json!({"transfer_id":self.id,"recipients":recipients,"envelope_sender":self.sender,"declared_size":self.size})).await.map_err(|_|failure)?;
        }
        let mut digest = Sha256::new();
        let mut total = 0;
        let mut sequence = 0u32;
        for chunk in prefix.chunks(SEND_CHUNK_SIZE) {
            buffer.payload()[..chunk.len()].copy_from_slice(chunk);
            self.chunk(buffer, chunk.len(), &mut digest, &mut total, &mut sequence)
                .await
                .map_err(|_| failure)?;
        }
        loop {
            let count = decoder
                .read(io, buffer.payload())
                .await
                .map_err(|_| failure)?;
            if count == 0 {
                break;
            }
            if total + count > p::MAX_EMAIL {
                return Err("552 5.3.4 Maximum message size exceeded");
            }
            self.chunk(buffer, count, &mut digest, &mut total, &mut sequence)
                .await
                .map_err(|_| failure)?;
        }
        let end = json!({"transfer_id":self.id,"total_bytes":total,"chunks":sequence,"sha256":hex::encode(digest.finalize())});
        let replies = join_all(self.peers.iter().map(|(route, (peer, _))| async {
            let reply = peer
                .ask(
                    p::END,
                    &end,
                    &self.id,
                    "",
                    broker.admission.settings().ack_timeout.as_secs(),
                )
                .await;
            match &reply {
                Ok(reply) if reply.kind == p::ACK => broker.admission.note_success(route),
                Err(error) if error.is::<crate::broker::ReplyTimeout>() => {
                    broker.admission.note_timeout(*route)
                }
                _ => {}
            }
            reply
        }))
        .await;
        if replies
            .into_iter()
            .any(|reply| !matches!(reply,Ok(reply) if reply.kind==p::ACK))
        {
            return Err(failure);
        }
        crate::event(
            "delivery_committed",
            json!({"bytes":total,"recipients":self.recipients.len(),"connections":self.peers.len()}),
        );
        Ok(())
    }

    async fn chunk(
        &self,
        buffer: &mut ChunkBuffer,
        count: usize,
        digest: &mut Sha256,
        total: &mut usize,
        sequence: &mut u32,
    ) -> Result<()> {
        *total += count;
        digest.update(&buffer.as_ref()[26..26 + count]);
        let frame = buffer.frame(self.bytes, *sequence, count);
        for (peer, _) in self.peers.values() {
            peer.frame(frame.clone()).await?;
        }
        *sequence += 1;
        Ok(())
    }

    async fn release(self, abort: bool) {
        if abort {
            let value = json!({"transfer_id":self.id});
            let _ = timeout(
                Duration::from_secs(3),
                join_all(
                    self.peers
                        .values()
                        .map(|(peer, _)| peer.send(p::ABORT, &value)),
                ),
            )
            .await;
        }
        for (peer, _) in self.peers.values() {
            peer.release(&self.id);
        }
    }
}

async fn response(io: &mut Socket, text: &str) -> Result<()> {
    timeout(Duration::from_secs(30), async {
        io.get_mut()
            .write_all(format!("{text}\r\n").as_bytes())
            .await?;
        io.get_mut().flush().await
    })
    .await??;
    Ok(())
}

async fn reset(transfer: &mut Option<Transfer>, abort: bool) {
    if let Some(transfer) = transfer.take() {
        transfer.release(abort).await;
    }
}

pub async fn serve(
    tcp: TcpStream,
    broker: Arc<Broker>,
    tls: TlsAcceptor,
    prefixes: Arc<Vec<IpNet>>,
) {
    let trusted = tcp
        .peer_addr()
        .is_ok_and(|addr| routing::trusted(addr.ip(), &prefixes));
    let mut transfer = None;
    // A session budget cannot be renewed by commands or STARTTLS.
    let result = tokio::select! {
        _ = broker.stopped.cancelled() => Ok(()),
        result = timeout(Duration::from_secs(1800),session(tcp,&broker,tls,trusted,&mut transfer)) => result.map(|_|()),
    };
    let _ = result;
    reset(&mut transfer, true).await;
}

async fn session(
    tcp: TcpStream,
    broker: &Broker,
    tls: TlsAcceptor,
    trusted: bool,
    transfer: &mut Option<Transfer>,
) -> Result<()> {
    tcp.set_nodelay(true)?;
    let mut io: Socket = BufReader::with_capacity(4096, Box::new(tcp));
    response(&mut io, "220 ahrara.dev ESMTP Service Ready").await?;
    let mut encrypted = false;
    let mut greeted = false;
    let mut errors = 0;
    // Allocate lazily and reuse for sequential deliveries on this bounded session.
    let mut buffer = ChunkBuffer::default();
    loop {
        let deadline = transfer
            .as_ref()
            .map(|tx| tx.deadline)
            .unwrap_or_else(|| Instant::now() + broker.admission.settings().command_timeout);
        let mut line = Vec::new();
        let count = timeout_at(
            deadline.min(Instant::now() + broker.admission.settings().command_timeout),
            (&mut io).take(2001).read_until(b'\n', &mut line),
        )
        .await??;
        if count == 0 {
            return Ok(());
        }
        if line.len() > 2000 || !line.ends_with(b"\n") {
            bail!("SMTP line too long");
        }
        let request = match Request::parse(&mut line.iter()) {
            Ok(request) => request.into_owned(),
            Err(_) => {
                response(&mut io, "501 5.5.2 Invalid command syntax").await?;
                errors += 1;
                if errors >= 3 {
                    return Ok(());
                }
                continue;
            }
        };
        match request {
            Request::Ehlo { .. } | Request::Helo { .. } => {
                reset(transfer, true).await;
                greeted = true;
                if matches!(request, Request::Ehlo { .. }) {
                    let starttls = if encrypted { "" } else { "250-STARTTLS\r\n" };
                    response(&mut io,&format!("250-ahrara.dev\r\n250-PIPELINING\r\n250-8BITMIME\r\n250-ENHANCEDSTATUSCODES\r\n{starttls}250-SIZE {}\r\n250 LIMITS RCPTMAX=100",p::MAX_EMAIL)).await?;
                } else {
                    response(&mut io, "250 2.0.0 Hello").await?;
                }
            }
            Request::StartTls => {
                if encrypted {
                    response(&mut io, "502 5.5.1 Already using TLS").await?;
                    continue;
                }
                response(&mut io, "220 2.0.0 Ready to start TLS").await?;
                reset(transfer, true).await;
                let secure =
                    timeout(Duration::from_secs(30), tls.accept(io.into_inner())).await??;
                io = BufReader::with_capacity(4096, Box::new(secure));
                encrypted = true;
                greeted = false;
            }
            Request::Mail { from } => {
                let unsupported = from.flags & !(MAIL_BODY_7BIT | MAIL_BODY_8BITMIME) != 0
                    || from.trans_id.is_some()
                    || from.by != 0
                    || from.env_id.is_some()
                    || from.solicit.is_some()
                    || from.mtrk.is_some()
                    || from.hold_for != 0
                    || from.hold_until != 0
                    || from.mt_priority != 0;
                let error = if !greeted {
                    Some("502 5.5.1 Please introduce yourself first")
                } else if unsupported {
                    Some("504 5.5.4 Unsupported MAIL parameter")
                } else if from.size > p::MAX_EMAIL {
                    Some("552 5.3.4 Maximum message size exceeded")
                } else if !encrypted {
                    Some("530 5.7.0 Must issue STARTTLS first")
                } else {
                    None
                };
                if let Some(error) = error {
                    response(&mut io, error).await?;
                    continue;
                }
                reset(transfer, true).await;
                *transfer = Some(Transfer::new(
                    from.address,
                    from.size,
                    broker.admission.settings().transaction_timeout,
                ));
                response(&mut io, "250 2.0.0 Sender accepted").await?;
            }
            Request::Rcpt { to } => {
                let Some(tx) = transfer.as_mut() else {
                    response(&mut io, "502 5.5.1 MAIL required").await?;
                    continue;
                };
                if to.flags != 0 || to.orcpt.is_some() || to.rrvs != 0 {
                    response(&mut io, "504 5.5.4 Unsupported RCPT parameter").await?;
                    continue;
                }
                let result = if trusted {
                    match auth::address(&to.address) {
                        Err(_) => Err("550 5.1.1 Mailbox unavailable"),
                        Ok((_, _, recipient))
                            if !tx.envelopes.contains(&recipient) && tx.envelopes.len() >= 100 =>
                        {
                            Err("452 4.5.3 Too many distinct recipients")
                        }
                        Ok((_, _, recipient)) => {
                            tx.envelopes.insert(recipient);
                            Ok(())
                        }
                    }
                } else {
                    timeout_at(tx.deadline, tx.offer(broker, &to.address)).await?
                };
                response(
                    &mut io,
                    result.err().unwrap_or("250 2.0.0 Recipient accepted"),
                )
                .await?;
            }
            Request::Data => {
                let Some(tx) = transfer.as_mut() else {
                    response(&mut io, "502 5.5.1 MAIL required").await?;
                    continue;
                };
                if tx.recipients.is_empty() && tx.envelopes.is_empty() {
                    response(&mut io, "502 5.5.1 RCPT required").await?;
                    continue;
                }
                response(&mut io, "354 End data with <CR><LF>.<CR><LF>").await?;
                let result =
                    timeout_at(tx.deadline, tx.data(&mut io, broker, trusted, &mut buffer)).await;
                buffer.clear();
                let reply = match result {
                    Ok(result) => result,
                    Err(_) => Err("451 4.4.2 Transaction expired"),
                };
                let failed = reply.is_err();
                reset(transfer, failed).await;
                response(
                    &mut io,
                    reply.err().unwrap_or("250 2.0.0 Delivery committed"),
                )
                .await?;
                // DATA failures terminate the connection, matching AbortOnDataError.
                if failed {
                    return Ok(());
                }
            }
            Request::Rset => {
                reset(transfer, true).await;
                response(&mut io, "250 2.0.0 Reset").await?;
            }
            Request::Noop { .. } => response(&mut io, "250 2.0.0 OK").await?,
            Request::Quit => {
                response(&mut io, "221 2.0.0 Bye").await?;
                return Ok(());
            }
            Request::Vrfy { .. } => response(&mut io, "252 2.5.2 Cannot verify user").await?,
            _ => response(&mut io, "502 5.5.1 Command not implemented").await?,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emitted_chunk_fits_one_authenticated_noise_record() {
        let key = snow::Builder::new(crate::secure::PATTERN.parse().unwrap())
            .generate_keypair()
            .unwrap();
        let mut client =
            crate::secure::initiator(key.public.as_slice().try_into().unwrap()).unwrap();
        let mut server =
            crate::secure::responder(key.private.as_slice().try_into().unwrap()).unwrap();
        let mut message = [0; crate::secure::MAX_HANDSHAKE];
        let mut payload = [0; crate::secure::MAX_HANDSHAKE];
        let length = client.write_message(&[], &mut message).unwrap();
        server
            .read_message(&message[..length], &mut payload)
            .unwrap();
        let length = server.write_message(&[], &mut message).unwrap();
        client
            .read_message(&message[..length], &mut payload)
            .unwrap();
        let (mut sender, _) = crate::secure::into_session(server).unwrap();
        let mut chunk = ChunkBuffer::default();
        let count = chunk.payload().len();
        let frame = chunk.frame([7; 16], 0, count);
        assert!(
            sender.seal(&frame).is_ok(),
            "SMTP emission exceeded the Noise record bound"
        );
        assert_eq!(p::decode(&frame).unwrap().1.len(), count + 20);
        assert_eq!(
            p::MAX_FRAME,
            65_562,
            "legacy receive limit must remain compatible"
        );
    }

    #[test]
    fn frames_reuse_payload_storage_without_sending_previous_tail() {
        let mut buffer = ChunkBuffer::default();
        buffer.payload().fill(0x41);
        let allocation = buffer.as_ref().as_ptr();
        let frame = buffer.frame([7; 16], 3, SEND_CHUNK_SIZE);
        assert_eq!(frame.as_ptr(), allocation);
        let (kind, payload) = p::decode(&frame).unwrap();
        assert_eq!(kind, p::CHUNK);
        assert_eq!(&payload[..16], &[7; 16]);
        assert_eq!(&payload[16..20], &3u32.to_be_bytes());
        assert!(payload[20..].iter().all(|byte| *byte == 0x41));
        drop(frame);

        buffer.clear();
        assert!(buffer.as_ref().iter().all(|byte| *byte == 0));
        buffer.payload()[..3].copy_from_slice(b"new");
        let frame = buffer.frame([8; 16], 0, 3);
        assert_eq!(frame.as_ptr(), allocation);
        assert_eq!(frame.len(), 29);
        assert_eq!(&p::decode(&frame).unwrap().1[20..], b"new");
    }

    #[test]
    fn outstanding_recipients_keep_immutable_bytes_after_reuse_and_cancellation() {
        let mut buffer = ChunkBuffer::default();
        buffer.payload()[..3].copy_from_slice(b"old");
        let pending = buffer.frame([1; 16], 0, 3);
        let second_recipient = pending.clone();
        let expected = pending.to_vec();

        // A transport retaining the previous frame forces a fresh payload buffer.
        buffer.payload()[..3].copy_from_slice(b"new");
        let next = buffer.frame([2; 16], 0, 3);
        assert_ne!(next.as_ptr(), pending.as_ptr());
        buffer.clear();
        assert!(buffer.as_ref().is_empty());
        drop(buffer);
        drop(pending);
        assert_eq!(second_recipient.as_ref(), expected);
        assert_eq!(&p::decode(&next).unwrap().1[20..], b"new");
    }
}
