use crate::{auth::decode_address, core::Core, protocol::*};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{SinkExt, StreamExt};
use rand::Rng;
use serde::Deserialize;
use serde_json::{Value, json as value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, io::BufReader, sync::Arc, time::Duration};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, connect_async_tls_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};
use tokio_util::sync::CancellationToken;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub async fn run(core: Arc<Core>, cancel: CancellationToken) {
    let mut attempt = 0u32;
    loop {
        let result = tokio::select! { _=cancel.cancelled()=>break, result=connect(&core)=>result };
        if core.state.borrow().connected {
            attempt = 0;
        }
        let error = result
            .err()
            .map(|e| e.to_string())
            .unwrap_or_else(|| "connection closed".into());
        if error == "identity_in_use" || error == "authentication_failed" {
            core.set_state("failed", Some(error)).await;
            return;
        }
        core.set_state("reconnecting", Some(error)).await;
        let base = (1u64 << attempt.min(5)).min(30);
        let jitter = rand::thread_rng().gen_range(0.8..1.0);
        let delay = Duration::from_secs_f64(base as f64 * jitter);
        attempt = attempt.saturating_add(1);
        tokio::select! { _=cancel.cancelled()=>break, _=tokio::time::sleep(delay)=>{} }
    }
    core.set_state("stopped", None).await;
}

async fn send(socket: &mut Socket, kind: u8, value: &Value) -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(30),
        socket.send(Message::Binary(json(kind, value)?.into())),
    )
    .await??;
    Ok(())
}
async fn receive(socket: &mut Socket) -> Result<Vec<u8>> {
    loop {
        match socket.next().await.context("connection closed")?? {
            Message::Binary(bytes) => return Ok(bytes.to_vec()),
            Message::Ping(bytes) => socket.send(Message::Pong(bytes)).await?,
            Message::Pong(_) => {}
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

async fn connect(core: &Arc<Core>) -> Result<()> {
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME))
        .max_frame_size(Some(MAX_FRAME))
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_FRAME * 2);
    let connector = Connector::Rustls(Arc::new(tls_config(core)?));
    let (mut socket, _) = tokio::time::timeout(
        Duration::from_secs(10),
        connect_async_tls_with_config(&core.config.server_url, Some(config), true, Some(connector)),
    )
    .await??;
    tokio::time::timeout(Duration::from_secs(15), async {
        send(
            &mut socket,
            HELLO,
            &value!({"public_key":URL_SAFE_NO_PAD.encode(core.identity.public_key())}),
        )
        .await?;
        let frame = receive(&mut socket).await?;
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
        let signature = core.identity.sign(&challenge)?;
        send(
            &mut socket,
            AUTHENTICATE,
            &value!({"signature":URL_SAFE_NO_PAD.encode(signature)}),
        )
        .await?;
        let frame = receive(&mut socket).await?;
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
        let frame = receive(&mut socket).await?;
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
                    &mut socket,
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
                // One allocation only while receiving; aliases share this buffer.
                d.raw = Vec::with_capacity(MAX_EMAIL);
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
                    .storage
                    .insert(
                        d.id.clone(),
                        core.identity.fingerprint(),
                        d.recipients,
                        d.sender,
                        d.raw,
                    )
                    .await;
                match saved {
                    Ok(emails) => {
                        core.received(emails);
                        send(&mut socket, ACK, &value!({"transfer_id":d.id})).await?;
                    }
                    Err(_) => {
                        send(
                            &mut socket,
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
