//! Typed streams on top of QUIC.
//!
//! Every stream starts with a one-byte [`StreamKind`] so the receiver can route it. Control-plane
//! streams then carry length-delimited protobuf ([`FramedStream`]); bulk and media streams carry
//! their own binary formats.

use bytes::BytesMut;
use iroh::endpoint::{RecvStream, SendStream};
use nexus_proto::framing::{FrameDecoder, encode_frame};
use prost::Message;

use crate::NetError;

/// What a stream carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum StreamKind {
    /// Session control: hello, RPC, notifications. Bidirectional, protobuf.
    Control = 1,
    /// Input events, controller → target. Bidirectional, high priority.
    Input = 2,
    /// Clipboard exchange.
    Clipboard = 3,
    /// File transfer.
    Files = 4,
    /// Terminal (PTY) session.
    Terminal = 5,
    /// TCP port-forward connection.
    PortForward = 6,
    /// One encoded video frame. Unidirectional.
    Video = 16,
    /// Diagnostics (echo / throughput). Used by `nexus ping` and tests.
    Diagnostics = 250,
}

impl StreamKind {
    /// Parses a kind byte.
    pub fn from_u8(v: u8) -> Result<Self, NetError> {
        Ok(match v {
            1 => Self::Control,
            2 => Self::Input,
            3 => Self::Clipboard,
            4 => Self::Files,
            5 => Self::Terminal,
            6 => Self::PortForward,
            16 => Self::Video,
            250 => Self::Diagnostics,
            _ => return Err(NetError::Protocol("unknown stream kind")),
        })
    }
}

/// A bidirectional QUIC stream.
#[derive(Debug)]
pub struct Stream {
    /// Sending half.
    pub send: SendStream,
    /// Receiving half.
    pub recv: RecvStream,
}

/// A bidirectional stream carrying length-delimited protobuf messages.
#[derive(Debug)]
pub struct FramedStream {
    stream: Stream,
    decoder: FrameDecoder,
    max_frame: usize,
}

impl FramedStream {
    /// Wraps `stream`, rejecting frames larger than `max_frame` bytes.
    #[must_use]
    pub fn new(stream: Stream, max_frame: usize) -> Self {
        Self {
            stream,
            decoder: FrameDecoder::new(max_frame),
            max_frame,
        }
    }

    /// Sends one message.
    pub async fn send<M: Message>(&mut self, msg: &M) -> Result<(), NetError> {
        let mut buf = BytesMut::new();
        encode_frame(msg, &mut buf, self.max_frame)?;
        self.stream
            .send
            .write_all(&buf)
            .await
            .map_err(NetError::stream)
    }

    /// Receives the next message; `Ok(None)` when the peer finished the stream cleanly.
    pub async fn recv<M: Message + Default>(&mut self) -> Result<Option<M>, NetError> {
        let mut chunk = vec![0u8; 16 * 1024];
        loop {
            if let Some(msg) = self.decoder.next_message::<M>()? {
                return Ok(Some(msg));
            }
            match self
                .stream
                .recv
                .read(&mut chunk)
                .await
                .map_err(NetError::stream)?
            {
                Some(n) => self.decoder.extend(&chunk[..n]),
                None if self.decoder.buffered() == 0 => return Ok(None),
                None => return Err(NetError::Protocol("stream ended inside a frame")),
            }
        }
    }

    /// Signals that no more messages will be sent.
    pub fn finish(&mut self) -> Result<(), NetError> {
        self.stream.send.finish().map_err(NetError::stream)
    }

    /// Gives back the raw stream (any buffered, undecoded bytes are discarded).
    #[must_use]
    pub fn into_inner(self) -> Stream {
        self.stream
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_kind_roundtrip_and_unknowns() {
        for kind in [
            StreamKind::Control,
            StreamKind::Input,
            StreamKind::Clipboard,
            StreamKind::Files,
            StreamKind::Terminal,
            StreamKind::PortForward,
            StreamKind::Video,
            StreamKind::Diagnostics,
        ] {
            assert_eq!(StreamKind::from_u8(kind as u8).unwrap(), kind);
        }
        for bad in [0u8, 7, 15, 17, 100, 255] {
            assert!(StreamKind::from_u8(bad).is_err(), "{bad}");
        }
    }
}
