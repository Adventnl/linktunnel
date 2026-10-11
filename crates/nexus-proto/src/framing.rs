//! Length-delimited framing for protobuf messages on reliable streams.
//!
//! Each frame is `varint(len) || body`. The decoder checks the declared length against a hard
//! cap *before* buffering the body, so a hostile peer cannot make us allocate gigabytes by
//! announcing a huge frame (a classic parser DoS).

use bytes::{Buf, Bytes, BytesMut};
use prost::Message;

/// Cap for control-plane frames (hello, RPC, small clipboard payloads): 1 MiB.
pub const MAX_CONTROL_FRAME: usize = 1 << 20;

/// Cap for bulk frames (file chunks, clipboard images): 8 MiB.
pub const MAX_BULK_FRAME: usize = 8 << 20;

/// A varint-encoded length takes at most this many bytes under our caps (< 2^35).
const MAX_VARINT_LEN: usize = 5;

/// Framing errors. Any of these means the stream is corrupt or hostile and must be closed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    /// The declared frame length exceeds the cap.
    #[error("frame of {len} bytes exceeds the {max} byte limit")]
    TooLarge {
        /// Declared length.
        len: u64,
        /// Configured maximum.
        max: usize,
    },
    /// The length prefix is not a valid varint.
    #[error("malformed length prefix")]
    BadVarint,
    /// The body is not a valid message of the expected type.
    #[error("malformed message: {0}")]
    Decode(String),
}

/// Appends `varint(len) || msg` to `out`, refusing messages larger than `max`.
pub fn encode_frame<M: Message>(msg: &M, out: &mut BytesMut, max: usize) -> Result<(), FrameError> {
    let len = msg.encoded_len();
    if len > max {
        return Err(FrameError::TooLarge {
            len: len as u64,
            max,
        });
    }
    out.reserve(len + MAX_VARINT_LEN);
    prost::encoding::encode_varint(len as u64, out);
    // `encode` only fails if the buffer is too small, and `BytesMut` grows on demand.
    msg.encode(out)
        .map_err(|e| FrameError::Decode(e.to_string()))
}

/// Incremental decoder: feed it bytes as they arrive, pull complete frames out.
#[derive(Debug)]
pub struct FrameDecoder {
    buf: BytesMut,
    max: usize,
}

impl FrameDecoder {
    /// Creates a decoder that rejects frames larger than `max` bytes.
    #[must_use]
    pub fn new(max: usize) -> Self {
        Self {
            buf: BytesMut::new(),
            max,
        }
    }

    /// Appends received bytes.
    pub fn extend(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Bytes currently buffered (partial frame included).
    #[must_use]
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// Returns the next complete frame body, `Ok(None)` if more bytes are needed.
    pub fn next_frame(&mut self) -> Result<Option<Bytes>, FrameError> {
        let Some((len, prefix_len)) = peek_varint(&self.buf)? else {
            return Ok(None);
        };
        if len > self.max as u64 {
            return Err(FrameError::TooLarge { len, max: self.max });
        }
        // `len <= max` and `max` is a `usize`, so this conversion cannot fail.
        let len = usize::try_from(len).map_err(|_| FrameError::TooLarge { len, max: self.max })?;
        if self.buf.len() < prefix_len + len {
            return Ok(None);
        }
        self.buf.advance(prefix_len);
        Ok(Some(self.buf.split_to(len).freeze()))
    }

    /// Returns the next complete message decoded as `M`.
    pub fn next_message<M: Message + Default>(&mut self) -> Result<Option<M>, FrameError> {
        match self.next_frame()? {
            Some(body) => M::decode(body)
                .map(Some)
                .map_err(|e| FrameError::Decode(e.to_string())),
            None => Ok(None),
        }
    }
}

/// Reads a varint from the front of `buf` without consuming it.
/// Returns `(value, bytes_used)`, `Ok(None)` if the varint is not complete yet.
fn peek_varint(buf: &[u8]) -> Result<Option<(u64, usize)>, FrameError> {
    let mut value: u64 = 0;
    for (i, byte) in buf.iter().take(MAX_VARINT_LEN).enumerate() {
        value |= u64::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return Ok(Some((value, i + 1)));
        }
    }
    if buf.len() >= MAX_VARINT_LEN {
        // Five continuation bytes: longer than any length we would ever accept.
        return Err(FrameError::BadVarint);
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::v1::{ControlMessage, Ping, control_message::Body};

    fn ping(id: u64, sent_us: u64) -> ControlMessage {
        ControlMessage {
            id,
            body: Some(Body::Ping(Ping { sent_us })),
            ..Default::default()
        }
    }

    #[test]
    fn roundtrip_single() {
        let mut wire = BytesMut::new();
        encode_frame(&ping(7, 99), &mut wire, MAX_CONTROL_FRAME).unwrap();
        let mut dec = FrameDecoder::new(MAX_CONTROL_FRAME);
        dec.extend(&wire);
        let got: ControlMessage = dec.next_message().unwrap().unwrap();
        assert_eq!(got, ping(7, 99));
        assert_eq!(dec.buffered(), 0);
        assert!(dec.next_message::<ControlMessage>().unwrap().is_none());
    }

    #[test]
    fn decoder_handles_arbitrary_chunking() {
        let mut wire = BytesMut::new();
        for i in 0..20 {
            encode_frame(&ping(i, i * 1000), &mut wire, MAX_CONTROL_FRAME).unwrap();
        }
        // Feed one byte at a time: the worst case for a streaming parser.
        let mut dec = FrameDecoder::new(MAX_CONTROL_FRAME);
        let mut out = Vec::new();
        for b in wire.iter() {
            dec.extend(&[*b]);
            while let Some(m) = dec.next_message::<ControlMessage>().unwrap() {
                out.push(m);
            }
        }
        assert_eq!(out.len(), 20);
        assert_eq!(out[13], ping(13, 13_000));
    }

    #[test]
    fn oversized_declared_length_is_rejected_before_buffering() {
        // varint(2 GiB) with no body at all.
        let mut dec = FrameDecoder::new(MAX_CONTROL_FRAME);
        dec.extend(&[0x80, 0x80, 0x80, 0x80, 0x08]);
        assert!(matches!(dec.next_frame(), Err(FrameError::TooLarge { .. })));
    }

    #[test]
    fn encode_refuses_oversized_messages() {
        let big = ControlMessage {
            body: Some(Body::Error(crate::v1::Error {
                code: 0,
                message: "x".repeat(2048),
            })),
            ..Default::default()
        };
        let mut out = BytesMut::new();
        assert!(matches!(
            encode_frame(&big, &mut out, 1024),
            Err(FrameError::TooLarge { .. })
        ));
        assert!(
            out.is_empty(),
            "nothing is written when encoding is refused"
        );
    }

    #[test]
    fn endless_continuation_bytes_are_rejected() {
        let mut dec = FrameDecoder::new(MAX_CONTROL_FRAME);
        dec.extend(&[0xff; 16]);
        assert_eq!(dec.next_frame().unwrap_err(), FrameError::BadVarint);
    }

    #[test]
    fn garbage_body_is_a_decode_error() {
        let mut dec = FrameDecoder::new(MAX_CONTROL_FRAME);
        // length 3, body is not valid protobuf (field 1 varint with truncated payload).
        dec.extend(&[0x03, 0x08, 0xff, 0xff]);
        assert!(matches!(
            dec.next_message::<ControlMessage>(),
            Err(FrameError::Decode(_))
        ));
    }

    proptest! {
        /// The decoder never panics on arbitrary bytes, and anything it yields respects the cap.
        #[test]
        fn decoder_is_total(data in proptest::collection::vec(any::<u8>(), 0..512)) {
            let mut dec = FrameDecoder::new(256);
            dec.extend(&data);
            while let Ok(Some(body)) = dec.next_frame() {
                prop_assert!(body.len() <= 256);
            }
        }

        #[test]
        fn roundtrip_any_ping(id in any::<u64>(), sent in any::<u64>()) {
            let mut wire = BytesMut::new();
            encode_frame(&ping(id, sent), &mut wire, MAX_CONTROL_FRAME).unwrap();
            let mut dec = FrameDecoder::new(MAX_CONTROL_FRAME);
            dec.extend(&wire);
            prop_assert_eq!(dec.next_message::<ControlMessage>().unwrap().unwrap(), ping(id, sent));
        }
    }
}
