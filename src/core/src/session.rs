//! The client side of the relay protocol, without I/O. Native and browser clients
//! feed it every received WebSocket message and send what it returns, so both run
//! the same authentication and delivery checks.
use crate::{
    auth::{Identity, decode_address},
    protocol::*,
    secure,
};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::{Value, json as value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// What the driver does after a received message.
pub enum Step {
    /// Send these bytes.
    Send(Vec<u8>),
    /// Solve `protocol::solve_work(&challenge, bits)` and pass the nonce to `Session::work`.
    Work {
        challenge: [u8; 32],
        bits: u8,
    },
    /// Authenticated; mail for this Identity is delivered from now on.
    Ready,
    /// Answer with `Session::offer` whether this local address is enabled.
    Offer {
        recipient: String,
    },
    /// Persist the message for every recipient, then answer with `Session::stored`.
    Deliver {
        transfer_id: String,
        recipients: Vec<String>,
        sender: String,
        raw: Vec<u8>,
    },
    Idle,
}

enum Phase {
    Handshake(Box<snow::HandshakeState>),
    Challenge {
        binding: [u8; 32],
    },
    Work {
        challenge: [u8; 32],
        bits: u8,
        signature: [u8; 64],
    },
    Authenticating,
    Ready,
}

/// One connection's protocol state. Feed messages in order and answer `Offer` and
/// `Deliver` before the next one: Noise nonces follow the order of sent messages.
pub struct Session {
    identity: Identity,
    phase: Phase,
    channel: Option<(secure::Sender, secure::Receiver)>,
    delivery: Option<Delivery>,
    offer: Option<Offer>,
    stored: Option<String>,
}

impl Session {
    /// Starts the Noise handshake with the pinned relay key; send the returned message first.
    pub fn open(identity: Identity, relay: &[u8; 32]) -> Result<(Self, Vec<u8>)> {
        let mut handshake = secure::initiator(relay)?;
        let mut message = [0; secure::MAX_HANDSHAKE];
        let length = handshake.write_message(&[], &mut message)?;
        Ok((
            Self {
                identity,
                phase: Phase::Handshake(Box::new(handshake)),
                channel: None,
                delivery: None,
                offer: None,
                stored: None,
            },
            message[..length].to_vec(),
        ))
    }

    pub fn receive(&mut self, packet: &[u8]) -> Result<Step> {
        if let Phase::Handshake(handshake) = &mut self.phase {
            let mut message = [0; secure::MAX_HANDSHAKE];
            if packet.len() > secure::MAX_HANDSHAKE
                || handshake.read_message(packet, &mut message)? != 0
                || !handshake.is_handshake_finished()
            {
                bail!("invalid encrypted transport handshake")
            }

            let binding = handshake.get_handshake_hash().try_into()?;
            let Phase::Handshake(handshake) =
                std::mem::replace(&mut self.phase, Phase::Challenge { binding })
            else {
                unreachable!()
            };
            self.channel = Some(secure::into_session(*handshake)?);
            let public = URL_SAFE_NO_PAD.encode(self.identity.public_key());
            return Ok(Step::Send(
                self.seal(HELLO, &value!({"public_key": public}))?,
            ));
        }

        let frame = self
            .channel
            .as_mut()
            .expect("handshake finished")
            .1
            .open(packet)?;
        let (kind, payload) = decode(&frame)?;
        ensure!(
            self.offer.is_none() && self.stored.is_none(),
            "message before answering the previous one"
        );
        match self.phase {
            Phase::Challenge { binding } => self.challenge(kind, payload, &binding),
            Phase::Authenticating => self.ready(kind, payload),
            Phase::Ready => self.deliver(kind, payload),
            Phase::Handshake(_) | Phase::Work { .. } => bail!("unexpected protocol frame"),
        }
    }

    pub fn work(&mut self, nonce: u64) -> Result<Vec<u8>> {
        let Phase::Work {
            challenge,
            bits,
            signature,
        } = self.phase
        else {
            bail!("no proof of work requested")
        };
        ensure!(
            crate::protocol::work(&challenge, nonce) >= u32::from(bits),
            "insufficient proof of work"
        );
        self.phase = Phase::Authenticating;
        self.seal(
            AUTHENTICATE,
            &value!({"signature": URL_SAFE_NO_PAD.encode(signature), "work": nonce}),
        )
    }

    pub fn offer(&mut self, accept: bool) -> Result<Vec<u8>> {
        let offer = self.offer.take().context("no pending offer")?;
        self.answer(offer, accept)
    }

    pub fn stored(&mut self, saved: bool) -> Result<Vec<u8>> {
        let id = self.stored.take().context("no pending delivery")?;
        if saved {
            self.seal(ACK, &value!({"transfer_id": id}))
        } else {
            self.seal(
                NACK,
                &value!({"transfer_id": id, "code": "persistence_failed"}),
            )
        }
    }

    /// Browsers cannot send WebSocket PINGs; the relay answers this frame instead.
    pub fn keepalive(&mut self) -> Result<Vec<u8>> {
        ensure!(
            matches!(self.phase, Phase::Ready),
            "keepalive before authentication"
        );
        let frame = encode(KEEPALIVE, &[])?;
        self.channel.as_mut().expect("authenticated").0.seal(&frame)
    }

    fn seal(&mut self, kind: u8, value: &Value) -> Result<Vec<u8>> {
        let frame = json(kind, value)?;
        self.channel
            .as_mut()
            .expect("handshake finished")
            .0
            .seal(&frame)
    }

    fn challenge(&mut self, kind: u8, payload: &[u8], binding: &[u8; 32]) -> Result<Step> {
        if kind != CHALLENGE {
            bail!("authentication_failed")
        }

        let request: Value = serde_json::from_slice(payload)?;
        let challenge: [u8; 32] = URL_SAFE_NO_PAD
            .decode(request["challenge"].as_str().context("missing challenge")?)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid authentication challenge"))?;
        let signature = self.identity.sign_bound(&challenge, binding)?;
        // A source holding many connections must prove work for each new one.
        let Some(bits) = request.get("work") else {
            self.phase = Phase::Authenticating;
            return Ok(Step::Send(self.seal(
                AUTHENTICATE,
                &value!({"signature": URL_SAFE_NO_PAD.encode(signature)}),
            )?));
        };
        let bits = bits
            .as_u64()
            .and_then(|bits| u8::try_from(bits).ok())
            .filter(|bits| *bits <= MAX_WORK)
            .context("unreasonable proof of work")?;
        self.phase = Phase::Work {
            challenge,
            bits,
            signature,
        };
        Ok(Step::Work { challenge, bits })
    }

    fn ready(&mut self, kind: u8, payload: &[u8]) -> Result<Step> {
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
            || response["route_id"] != hex::encode(self.identity.route())
            || response["fingerprint"] != self.identity.fingerprint()
        {
            bail!("authentication_failed")
        }

        self.phase = Phase::Ready;
        Ok(Step::Ready)
    }

    fn deliver(&mut self, kind: u8, payload: &[u8]) -> Result<Step> {
        match kind {
            OFFER => {
                let offer: Offer = serde_json::from_slice(payload)?;
                let valid =
                    decode_address(&offer.recipient).is_ok_and(|(route, alias, canonical)| {
                        route == self.identity.route()
                            && canonical == offer.recipient
                            && hex::encode(alias) == offer.alias_id
                    });
                let available = self.delivery.as_ref().is_none_or(|d| {
                    d.id == offer.transfer_id
                        && !d.started
                        && d.accepted.len() < 100
                        && d.sender == offer.envelope_sender
                });
                if !valid || !available || offer.declared_size.is_some_and(|n| n > MAX_EMAIL as u64)
                {
                    return Ok(Step::Send(self.answer(offer, false)?));
                }

                let recipient = offer.recipient.clone();
                self.offer = Some(offer);
                Ok(Step::Offer { recipient })
            }
            START => {
                let start: Start = serde_json::from_slice(payload)?;
                let d = self
                    .delivery
                    .as_mut()
                    .context("start without accepted offer")?;
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
                Ok(Step::Idle)
            }
            CHUNK => {
                self.delivery
                    .as_mut()
                    .context("chunk without transfer")?
                    .chunk(payload)?;
                Ok(Step::Idle)
            }
            END => {
                let end: End = serde_json::from_slice(payload)?;
                let d = self.delivery.take().context("end without transfer")?;
                d.validate_end(&end)?;
                self.stored = Some(d.id.clone());
                Ok(Step::Deliver {
                    transfer_id: d.id,
                    recipients: d.recipients,
                    sender: d.sender,
                    raw: d.raw,
                })
            }
            ABORT => {
                let value: Value = serde_json::from_slice(payload)?;
                if self
                    .delivery
                    .as_ref()
                    .is_some_and(|d| value["transfer_id"] == d.id)
                {
                    self.delivery = None;
                }

                Ok(Step::Idle)
            }
            KEEPALIVE if payload.is_empty() => Ok(Step::Idle),
            _ => bail!("unexpected protocol frame"),
        }
    }

    fn answer(&mut self, offer: Offer, accept: bool) -> Result<Vec<u8>> {
        if accept {
            if self.delivery.is_none() {
                self.delivery = Some(Delivery::new(&offer)?);
            }

            self.delivery
                .as_mut()
                .expect("delivery exists")
                .accepted
                .insert(offer.recipient.clone());
        }

        self.seal(
            if accept { ACCEPT } else { REJECT },
            &value!({"transfer_id": offer.transfer_id, "recipient": offer.recipient}),
        )
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signature, VerifyingKey};

    /// The relay side of one connection, built from the same secure transport.
    struct Relay {
        handshake: Option<snow::HandshakeState>,
        channel: Option<(secure::Sender, secure::Receiver)>,
        binding: [u8; 32],
    }

    impl Relay {
        fn accept(&mut self, message: &[u8]) -> Vec<u8> {
            let mut state = self.handshake.take().unwrap();
            let mut buffer = [0; secure::MAX_HANDSHAKE];

            assert_eq!(state.read_message(message, &mut buffer).unwrap(), 0);
            let length = state.write_message(&[], &mut buffer).unwrap();
            self.binding = state.get_handshake_hash().try_into().unwrap();
            self.channel = Some(secure::into_session(state).unwrap());
            buffer[..length].to_vec()
        }

        fn send(&mut self, kind: u8, payload: &[u8]) -> Vec<u8> {
            let frame = encode(kind, payload).unwrap();
            self.channel.as_mut().unwrap().0.seal(&frame).unwrap()
        }

        fn json(&mut self, kind: u8, value: Value) -> Vec<u8> {
            self.send(kind, &serde_json::to_vec(&value).unwrap())
        }

        fn open(&mut self, packet: &[u8]) -> (u8, Value) {
            let frame = self.channel.as_mut().unwrap().1.open(packet).unwrap();
            let (kind, payload) = decode(&frame).unwrap();
            if payload.is_empty() {
                return (kind, Value::Null);
            }

            (kind, serde_json::from_slice(payload).unwrap())
        }
    }

    fn sent(step: Step) -> Vec<u8> {
        let Step::Send(packet) = step else {
            panic!("expected a message to send")
        };

        packet
    }

    /// Authenticates up to READY, answering any requested proof of work.
    fn login(work: Option<u8>) -> (Identity, Session, Relay) {
        let identity = Identity::generate();
        let key = snow::Builder::new(secure::PATTERN.parse().unwrap())
            .generate_keypair()
            .unwrap();
        let mut relay = Relay {
            handshake: Some(secure::responder(key.private.as_slice().try_into().unwrap()).unwrap()),
            channel: None,
            binding: [0; 32],
        };
        let (mut session, handshake) =
            Session::open(identity.clone(), key.public.as_slice().try_into().unwrap()).unwrap();
        let response = relay.accept(&handshake);
        let (kind, hello) = relay.open(&sent(session.receive(&response).unwrap()));

        assert_eq!(kind, HELLO);
        assert_eq!(
            hello["public_key"],
            URL_SAFE_NO_PAD.encode(identity.public_key())
        );
        let challenge = [5; 32];
        let mut request = value!({"challenge": URL_SAFE_NO_PAD.encode(challenge)});
        if let Some(bits) = work {
            request["work"] = bits.into();
        }

        let packet = match session.receive(&relay.json(CHALLENGE, request)).unwrap() {
            Step::Send(packet) => packet,
            Step::Work {
                challenge: asked,
                bits,
            } => {
                assert_eq!((asked, Some(bits)), (challenge, work));
                let weak = (0..)
                    .find(|&nonce| crate::protocol::work(&challenge, nonce) < u32::from(bits))
                    .unwrap();

                assert!(session.work(weak).is_err(), "insufficient work was sent");
                session.work(solve_work(&challenge, bits)).unwrap()
            }
            _ => panic!("expected AUTHENTICATE"),
        };
        let (kind, signed) = relay.open(&packet);

        assert_eq!(kind, AUTHENTICATE);
        let signature = URL_SAFE_NO_PAD
            .decode(signed["signature"].as_str().unwrap())
            .unwrap();
        VerifyingKey::from_bytes(&identity.public_key())
            .unwrap()
            .verify_strict(
                &secure::auth_preimage(&identity.public_key(), &challenge, &relay.binding),
                &Signature::from_slice(&signature).unwrap(),
            )
            .unwrap();
        match work {
            Some(bits) => assert!(
                crate::protocol::work(&challenge, signed["work"].as_u64().unwrap())
                    >= u32::from(bits)
            ),
            None => assert!(signed.get("work").is_none()),
        }

        (identity, session, relay)
    }

    fn ready(identity: &Identity, session: &mut Session, relay: &mut Relay) {
        let packet = relay.json(
            READY,
            value!({"route_id": hex::encode(identity.route()), "fingerprint": identity.fingerprint()}),
        );

        assert!(matches!(session.receive(&packet).unwrap(), Step::Ready));
    }

    #[test]
    fn authentication_binds_the_handshake_and_proves_requested_work() {
        for work in [None, Some(12)] {
            let (identity, mut session, mut relay) = login(work);
            ready(&identity, &mut session, &mut relay);
        }
    }

    #[test]
    fn ready_must_match_this_identity_and_failures_keep_their_code() {
        let (_, mut session, mut relay) = login(None);
        let other = Identity::generate();
        let packet = relay.json(
            READY,
            value!({"route_id": hex::encode(other.route()), "fingerprint": other.fingerprint()}),
        );

        assert_eq!(
            session.receive(&packet).err().unwrap().to_string(),
            "authentication_failed"
        );
        for (code, error) in [
            ("identity_in_use", "identity_in_use"),
            ("invalid_hello", "authentication_failed"),
        ] {
            let (_, mut session, mut relay) = login(None);
            let packet = relay.json(FAILURE, value!({"code": code}));

            assert_eq!(session.receive(&packet).err().unwrap().to_string(), error);
        }
    }

    #[test]
    fn offers_for_other_identities_or_aliases_are_rejected_at_once() {
        let (identity, mut session, mut relay) = login(None);
        ready(&identity, &mut session, &mut relay);
        let alias = [1; 8];
        let recipient = crate::auth::address(&identity.route(), &alias);
        let transfer = hex::encode([9; 16]);
        let offer = |recipient: &str, alias: [u8; 8]| {
            value!({
                "transfer_id": transfer,
                "recipient": recipient,
                "alias_id": hex::encode(alias),
                "envelope_sender": "sender@example.test",
                "declared_size": null
            })
        };
        let foreign = crate::auth::address(&Identity::generate().route(), &alias);

        for (address, alias) in [(foreign.as_str(), alias), (recipient.as_str(), [2; 8])] {
            let step = session
                .receive(&relay.json(OFFER, offer(address, alias)))
                .unwrap();

            assert_eq!(relay.open(&sent(step)).0, REJECT);
        }
        let Step::Offer { recipient: asked } = session
            .receive(&relay.json(OFFER, offer(&recipient, alias)))
            .unwrap()
        else {
            panic!("a valid offer needs the local address decision")
        };

        assert_eq!(asked, recipient);
        assert!(
            session.receive(&relay.send(KEEPALIVE, &[])).is_err(),
            "a message arrived before the offer was answered"
        );
    }

    #[test]
    fn accepted_mail_is_delivered_once_validated_then_acknowledged() {
        let (identity, mut session, mut relay) = login(None);
        ready(&identity, &mut session, &mut relay);
        let alias = [1; 8];
        let recipient = crate::auth::address(&identity.route(), &alias);
        let transfer = [9; 16];
        let offer = value!({
            "transfer_id": hex::encode(transfer),
            "recipient": recipient,
            "alias_id": hex::encode(alias),
            "envelope_sender": "sender@example.test",
            "declared_size": 6
        });

        assert!(matches!(
            session.receive(&relay.json(OFFER, offer)).unwrap(),
            Step::Offer { .. }
        ));
        let (kind, accepted) = relay.open(&session.offer(true).unwrap());

        assert_eq!(kind, ACCEPT);
        assert_eq!(accepted["recipient"], recipient);
        let start = value!({
            "transfer_id": hex::encode(transfer),
            "recipients": [recipient],
            "envelope_sender": "sender@example.test",
            "declared_size": 6
        });

        assert!(matches!(
            session.receive(&relay.json(START, start)).unwrap(),
            Step::Idle
        ));
        let mut chunk = transfer.to_vec();
        chunk.extend(0u32.to_be_bytes());
        chunk.extend(b"body\r\n");

        assert!(matches!(
            session.receive(&relay.send(CHUNK, &chunk)).unwrap(),
            Step::Idle
        ));
        let end = value!({
            "transfer_id": hex::encode(transfer),
            "total_bytes": 6,
            "chunks": 1,
            "sha256": hex::encode(Sha256::digest(b"body\r\n"))
        });
        let Step::Deliver {
            transfer_id,
            recipients,
            sender,
            raw,
        } = session.receive(&relay.json(END, end)).unwrap()
        else {
            panic!("a validated END delivers the message")
        };

        assert_eq!(
            (transfer_id.as_str(), recipients, sender.as_str(), raw),
            (
                hex::encode(transfer).as_str(),
                vec![recipient],
                "sender@example.test",
                b"body\r\n".to_vec()
            )
        );
        let (kind, ack) = relay.open(&session.stored(true).unwrap());

        assert_eq!(
            (kind, ack),
            (ACK, value!({"transfer_id": hex::encode(transfer)}))
        );
        assert!(matches!(
            session.receive(&relay.send(KEEPALIVE, &[])).unwrap(),
            Step::Idle
        ));
        assert_eq!(
            relay.open(&session.keepalive().unwrap()),
            (KEEPALIVE, Value::Null)
        );
    }

    #[test]
    fn abort_releases_the_delivery_for_the_next_transfer() {
        let (identity, mut session, mut relay) = login(None);
        ready(&identity, &mut session, &mut relay);
        let recipient = crate::auth::address(&identity.route(), &[1; 8]);
        for transfer in [[1u8; 16], [2; 16]] {
            let offer = value!({
                "transfer_id": hex::encode(transfer),
                "recipient": recipient,
                "alias_id": hex::encode([1u8; 8]),
                "envelope_sender": "sender@example.test",
                "declared_size": null
            });

            assert!(matches!(
                session.receive(&relay.json(OFFER, offer)).unwrap(),
                Step::Offer { .. }
            ));
            assert_eq!(relay.open(&session.offer(true).unwrap()).0, ACCEPT);
            let abort = relay.json(ABORT, value!({"transfer_id": hex::encode(transfer)}));

            assert!(matches!(session.receive(&abort).unwrap(), Step::Idle));
        }
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
