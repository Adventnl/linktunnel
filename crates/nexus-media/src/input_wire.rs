//! Length-prefixed [`InputPacket`]s on the input stream.
//!
//! The input stream is reliable and ordered, so key events can never be reordered or lost.
//! Each packet is `len:u32` (little endian) followed by the packet bytes.

use bytes::{BufMut, BytesMut};
use nexus_proto::wire::input::InputPacket;

use crate::MediaError;

/// Largest accepted packet body. A packet holds at most 64 events of at most ~1 KiB.
pub const MAX_PACKET_BYTES: usize = 128 * 1024;

/// Encodes `packet` with its length prefix.
pub fn encode(packet: &InputPacket) -> Result<BytesMut, MediaError> {
    let mut body = BytesMut::new();
    packet
        .encode(&mut body)
        .map_err(|e| MediaError::Protocol(format!("input packet: {e}")))?;
    if body.len() > MAX_PACKET_BYTES {
        return Err(MediaError::Protocol("input packet too large".into()));
    }
    let mut out = BytesMut::with_capacity(body.len() + 4);
    out.put_u32_le(
        u32::try_from(body.len())
            .map_err(|_| MediaError::Protocol("input packet too large".into()))?,
    );
    out.extend_from_slice(&body);
    Ok(out)
}

/// Incremental decoder for a stream of length-prefixed packets.
#[derive(Debug, Default)]
pub struct PacketReader {
    buf: BytesMut,
}

impl PacketReader {
    /// Creates an empty reader.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds bytes received from the stream.
    pub fn extend(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Bytes buffered but not yet consumed.
    #[must_use]
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// The next complete packet, `Ok(None)` if more bytes are needed. A malformed length or
    /// body is an error and the stream must be closed: there is no way to resynchronise.
    pub fn next_packet(&mut self) -> Result<Option<InputPacket>, MediaError> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_le_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
        if len > MAX_PACKET_BYTES {
            return Err(MediaError::Protocol(format!(
                "input packet of {len} bytes exceeds the limit"
            )));
        }
        if self.buf.len() < 4 + len {
            return Ok(None);
        }
        let _ = self.buf.split_to(4);
        let body = self.buf.split_to(len);
        InputPacket::decode(&body)
            .map(Some)
            .map_err(|e| MediaError::Protocol(format!("input packet: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use nexus_proto::wire::input::{InputEvent, KeyState, Modifiers, TimedInput};

    use super::*;

    fn packet(seq: u32, n: usize) -> InputPacket {
        InputPacket {
            seq,
            base_us: 99,
            events: (0..n)
                .map(|i| TimedInput {
                    dt_us: u32::try_from(i).unwrap(),
                    event: InputEvent::Key {
                        page: 7,
                        usage: 4 + u16::try_from(i).unwrap(),
                        state: KeyState::Down,
                        modifiers: Modifiers::default(),
                    },
                })
                .collect(),
            skipped: 0,
        }
    }

    #[test]
    fn packets_survive_arbitrary_chunking() {
        let mut wire = BytesMut::new();
        for seq in 0..5 {
            wire.extend_from_slice(&encode(&packet(seq * 3, 3)).unwrap());
        }
        for chunk in [1usize, 2, 3, 7, 64, 1000] {
            let mut reader = PacketReader::new();
            let mut got = Vec::new();
            for piece in wire.chunks(chunk) {
                reader.extend(piece);
                while let Some(p) = reader.next_packet().unwrap() {
                    got.push(p.seq);
                }
            }
            assert_eq!(got, vec![0, 3, 6, 9, 12], "chunk size {chunk}");
            assert_eq!(reader.buffered(), 0);
        }
    }

    #[test]
    fn oversized_length_prefix_is_rejected_before_buffering() {
        let mut reader = PacketReader::new();
        reader.extend(&u32::MAX.to_le_bytes());
        assert!(reader.next_packet().is_err());
    }

    #[test]
    fn garbage_body_is_an_error() {
        let mut reader = PacketReader::new();
        reader.extend(&8u32.to_le_bytes());
        reader.extend(&[0xFF; 8]);
        assert!(reader.next_packet().is_err());
    }
}
