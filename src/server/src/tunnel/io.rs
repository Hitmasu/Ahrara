//! A byte stream over one HTTP/2 request/response pair from the edge.
//!
//! Derived from libcfd 0.2.0 (Apache-2.0, see `LICENSE`). Carries the
//! registration RPC and every WebSocket the edge opens toward the relay.

use futures_util::io::{AsyncRead, AsyncWrite};
use h2::{RecvStream, SendStream};
use std::{
    io,
    pin::Pin,
    task::{Context, Poll, ready},
};
use tokio_util::bytes::{Buf, Bytes};

pub struct H2Stream {
    receive: RecvStream,
    chunk: Bytes,
    send: SendStream<Bytes>,
    closed: bool,
}

impl H2Stream {
    pub fn new(receive: RecvStream, send: SendStream<Bytes>) -> Self {
        Self {
            receive,
            chunk: Bytes::new(),
            send,
            closed: false,
        }
    }

    fn end(&mut self) {
        if !self.closed {
            self.closed = true;
            let _ = self.send.send_data(Bytes::new(), true);
        }
    }
}

impl AsyncRead for H2Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        while self.chunk.is_empty() {
            match ready!(self.receive.poll_data(cx)) {
                Some(Ok(chunk)) => self.chunk = chunk,
                Some(Err(error)) => return Poll::Ready(Err(io::Error::other(error))),
                None => return Poll::Ready(Ok(0)),
            }
        }
        let size = buffer.len().min(self.chunk.len());
        buffer[..size].copy_from_slice(&self.chunk[..size]);
        self.chunk.advance(size);
        // Reopen the window only for consumed bytes, bounding what the edge may buffer here.
        self.receive
            .flow_control()
            .release_capacity(size)
            .map_err(io::Error::other)?;
        Poll::Ready(Ok(size))
    }
}

impl AsyncWrite for H2Stream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.closed {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        if buffer.is_empty() {
            return Poll::Ready(Ok(0));
        }
        // `send_data` buffers without bound unless capacity is reserved first.
        if self.send.capacity() == 0 {
            self.send.reserve_capacity(buffer.len());
            match ready!(self.send.poll_capacity(cx)) {
                Some(Ok(_)) => {}
                Some(Err(error)) => return Poll::Ready(Err(io::Error::other(error))),
                None => return Poll::Ready(Err(io::ErrorKind::ConnectionReset.into())),
            }
        }
        let size = buffer.len().min(self.send.capacity());
        self.send
            .send_data(Bytes::copy_from_slice(&buffer[..size]), false)
            .map_err(io::Error::other)?;
        Poll::Ready(Ok(size))
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.end();
        Poll::Ready(Ok(()))
    }
}

impl Drop for H2Stream {
    // Like a TCP close: queued frames such as an authentication FAILURE reach the
    // peer before END_STREAM, instead of h2's implicit CANCEL reset discarding them.
    fn drop(&mut self) {
        self.end();
    }
}
