//! Audio packet framing (sent as unreliable QUIC datagrams).
//!
//! Frames are tiny (2.5-10 ms of Opus is ~60-250 bytes), so one packet is one datagram and
//! loss is concealed by the decoder rather than retransmitted.
//!
//! ```text
//!  off  size  field
//!    0     1  wire version (1)
//!    1     1  codec    (1 = Opus, 2 = PCM s16le — used for tests / loopback)
//!    2     1  channels (1 or 2)
//!    3     1  flags    (bit0: discontinuity — sender dropped audio before this packet)
//!    4     2  seq      (wraps; receiver uses serial arithmetic)
//!    6     2  frame_samples (samples per channel in this packet, at 48 kHz)
//!    8     4  timestamp (48 kHz sample clock of the first sample, wraps)
//!   12     …  payload
//! ```

use bytes::BufMut;

use super::{Reader, WIRE_VERSION, WireError};

/// Header size.
pub const HEADER_LEN: usize = 12;
/// A whole packet must fit in one datagram at the conservative 1200-byte QUIC MTU.
pub const MAX_PACKET_LEN: usize = 1200;

/// Audio bitstream formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    /// Opus, 48 kHz.
    Opus,
    /// Raw signed 16-bit little-endian PCM, 48 kHz (testing and loopback only).
    PcmS16,
}

impl AudioCodec {
    const fn to_u8(self) -> u8 {
        match self {
            Self::Opus => 1,
            Self::PcmS16 => 2,
        }
    }

    fn from_u8(v: u8) -> Result<Self, WireError> {
        match v {
            1 => Ok(Self::Opus),
            2 => Ok(Self::PcmS16),
            other => Err(WireError::Unknown {
                what: "audio codec",
                value: u32::from(other),
            }),
        }
    }
}

/// Flag: the sender dropped audio immediately before this packet.
pub const FLAG_DISCONTINUITY: u8 = 1;

/// Parsed audio packet header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioHeader {
    /// Payload format.
    pub codec: AudioCodec,
    /// 1 (mono) or 2 (stereo).
    pub channels: u8,
    /// Flag bits (see `FLAG_*`).
    pub flags: u8,
    /// Packet counter.
    pub seq: u16,
    /// Samples per channel carried.
    pub frame_samples: u16,
    /// Sample-clock timestamp of the first sample.
    pub timestamp: u32,
}

/// Encodes `header` + `payload` into `out`. Fails if the packet would not fit one datagram.
pub fn encode_packet(
    header: &AudioHeader,
    payload: &[u8],
    out: &mut impl BufMut,
) -> Result<(), WireError> {
    if !(1..=2).contains(&header.channels) {
        return Err(WireError::Invalid("channel count"));
    }
    let total = HEADER_LEN + payload.len();
    if total > MAX_PACKET_LEN {
        return Err(WireError::TooLarge {
            what: "audio packet",
            len: total,
            max: MAX_PACKET_LEN,
        });
    }
    out.put_u8(WIRE_VERSION);
    out.put_u8(header.codec.to_u8());
    out.put_u8(header.channels);
    out.put_u8(header.flags);
    out.put_u16_le(header.seq);
    out.put_u16_le(header.frame_samples);
    out.put_u32_le(header.timestamp);
    out.put_slice(payload);
    Ok(())
}

/// Splits a received datagram into header and payload (borrowed from `buf`).
pub fn decode_packet(buf: &[u8]) -> Result<(AudioHeader, &[u8]), WireError> {
    if buf.len() > MAX_PACKET_LEN {
        return Err(WireError::TooLarge {
            what: "audio packet",
            len: buf.len(),
            max: MAX_PACKET_LEN,
        });
    }
    let mut r = Reader::new(buf);
    let version = r.u8()?;
    if version != WIRE_VERSION {
        return Err(WireError::BadVersion(version));
    }
    let codec = AudioCodec::from_u8(r.u8()?)?;
    let channels = r.u8()?;
    if !(1..=2).contains(&channels) {
        return Err(WireError::Invalid("channel count"));
    }
    let flags = r.u8()?;
    let seq = r.u16()?;
    let frame_samples = r.u16()?;
    let timestamp = r.u32()?;
    let payload = r.bytes(r.remaining())?;
    Ok((
        AudioHeader {
            codec,
            channels,
            flags,
            seq,
            frame_samples,
            timestamp,
        },
        payload,
    ))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn header() -> AudioHeader {
        AudioHeader {
            codec: AudioCodec::Opus,
            channels: 2,
            flags: FLAG_DISCONTINUITY,
            seq: 65_535,
            frame_samples: 480,
            timestamp: 4_000_000_000,
        }
    }

    #[test]
    fn roundtrip() {
        let mut buf = Vec::new();
        encode_packet(&header(), &[1, 2, 3, 4, 5], &mut buf).unwrap();
        assert_eq!(buf.len(), HEADER_LEN + 5);
        let (h, payload) = decode_packet(&buf).unwrap();
        assert_eq!(h, header());
        assert_eq!(payload, &[1, 2, 3, 4, 5]);
    }

    #[test]
    fn empty_payload_is_legal_for_silence_suppression() {
        let mut buf = Vec::new();
        encode_packet(&header(), &[], &mut buf).unwrap();
        let (_, payload) = decode_packet(&buf).unwrap();
        assert!(payload.is_empty());
    }

    #[test]
    fn oversized_packets_are_refused_both_ways() {
        let big = vec![0u8; MAX_PACKET_LEN];
        assert!(matches!(
            encode_packet(&header(), &big, &mut Vec::new()),
            Err(WireError::TooLarge { .. })
        ));
        assert!(matches!(
            decode_packet(&vec![1u8; MAX_PACKET_LEN + 1]),
            Err(WireError::TooLarge { .. })
        ));
    }

    #[test]
    fn rejects_bad_fields() {
        let mut buf = Vec::new();
        encode_packet(&header(), &[0], &mut buf).unwrap();
        let mut bad = buf.clone();
        bad[0] = 2;
        assert_eq!(decode_packet(&bad).unwrap_err(), WireError::BadVersion(2));
        let mut bad = buf.clone();
        bad[1] = 9;
        assert!(matches!(
            decode_packet(&bad),
            Err(WireError::Unknown { .. })
        ));
        let mut bad = buf;
        bad[2] = 0;
        assert_eq!(
            decode_packet(&bad).unwrap_err(),
            WireError::Invalid("channel count")
        );
    }

    proptest! {
        #[test]
        fn decode_never_panics(data in proptest::collection::vec(any::<u8>(), 0..1300)) {
            let _ = decode_packet(&data);
        }

        #[test]
        fn roundtrip_arbitrary(
            ch in 1u8..=2, flags in any::<u8>(), seq in any::<u16>(), fs in any::<u16>(),
            ts in any::<u32>(), payload in proptest::collection::vec(any::<u8>(), 0..=MAX_PACKET_LEN - HEADER_LEN),
        ) {
            let h = AudioHeader { codec: AudioCodec::PcmS16, channels: ch, flags, seq, frame_samples: fs, timestamp: ts };
            let mut buf = Vec::new();
            encode_packet(&h, &payload, &mut buf).unwrap();
            let (h2, p2) = decode_packet(&buf).unwrap();
            prop_assert_eq!(h2, h);
            prop_assert_eq!(p2, payload.as_slice());
        }
    }
}
