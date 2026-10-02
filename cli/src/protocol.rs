use anyhow::{Result, bail};
use serde::Serialize;

pub const MAX_EMAIL: usize = 10 * 1024 * 1024;
pub const CHUNK_SIZE: usize = 64 * 1024;
pub const MAX_CONTROL: usize = 16 * 1024;
pub const MAX_FRAME: usize = 6 + 20 + CHUNK_SIZE;
pub const HELLO: u8 = 1;
pub const CHALLENGE: u8 = 2;
pub const AUTHENTICATE: u8 = 3;
pub const READY: u8 = 4;
pub const FAILURE: u8 = 5;
pub const OFFER: u8 = 10;
pub const ACCEPT: u8 = 11;
pub const REJECT: u8 = 12;
pub const START: u8 = 20;
pub const CHUNK: u8 = 21;
pub const END: u8 = 22;
pub const ABORT: u8 = 23;
pub const ACK: u8 = 24;
pub const NACK: u8 = 25;

pub fn encode(kind: u8, payload: &[u8]) -> Result<Vec<u8>> {
    let limit = if kind == CHUNK {
        20 + CHUNK_SIZE
    } else {
        MAX_CONTROL
    };
    if payload.len() > limit {
        bail!("protocol frame too large")
    }
    let mut frame = Vec::with_capacity(6 + payload.len());
    frame.extend([1, kind]);
    frame.extend((payload.len() as u32).to_be_bytes());
    frame.extend(payload);
    Ok(frame)
}
pub fn json(kind: u8, value: &impl Serialize) -> Result<Vec<u8>> {
    encode(kind, &serde_json::to_vec(value)?)
}
pub fn decode(frame: &[u8]) -> Result<(u8, &[u8])> {
    if frame.len() < 6
        || frame[0] != 1
        || u32::from_be_bytes(frame[2..6].try_into()?) as usize != frame.len() - 6
    {
        bail!("invalid protocol frame")
    }
    let limit = if frame[1] == CHUNK {
        20 + CHUNK_SIZE
    } else {
        MAX_CONTROL
    };
    if frame.len() - 6 > limit {
        bail!("protocol frame too large")
    }
    Ok((frame[1], &frame[6..]))
}
