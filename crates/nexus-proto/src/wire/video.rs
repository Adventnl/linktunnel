//! Video frame header.
//!
//! Each encoded frame travels on its own QUIC unidirectional stream (see ADR-0008): a 40-byte
//! fixed header, `dirty_count` 8-byte rectangles, then `payload_len` bytes of codec bitstream.
//! A per-frame stream gives loss recovery inside the frame for free, and lets the sender
//! `reset()` a stream whose frame has already gone stale instead of delivering it late.
//!
//! ```text
//!  off  size  field
//!    0     1  wire version (1)
//!    1     1  codec        (nexus.v1.VideoCodec value)
//!    2     1  kind         (0 delta, 1 key, 2 intra-refresh)
//!    3     1  monitor index
//!    4     2  flags
//!    6     2  width   (pixels)
//!    8     2  height  (pixels)
//!   10     2  dirty rectangle count (<= 64)
//!   12     4  frame_seq
//!   16     4  ref_seq  (frame this one predicts from; == frame_seq for intra frames)
//!   20     4  payload_len
//!   24     8  capture_us (sender monotonic clock)
//!   32     8  send_us    (sender monotonic clock, when the last byte left the encoder)
//! ```

use bytes::BufMut;

use super::{Reader, WIRE_VERSION, WireError};
use crate::v1::VideoCodec;

/// Size of the fixed part of the header.
pub const FIXED_LEN: usize = 40;
/// Size of one dirty rectangle.
pub const RECT_LEN: usize = 8;
/// Most rectangles a header may carry; senders merge into a bounding box beyond this.
pub const MAX_DIRTY_RECTS: usize = 64;
/// Largest accepted codec payload (a 8K intra frame is well below this).
pub const MAX_PAYLOAD: u32 = 64 << 20;

/// What a frame depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// Predicted from `ref_seq`.
    Delta,
    /// Self-contained (IDR).
    Key,
    /// A delta frame that also refreshes a slice of the picture, so the stream heals after loss
    /// without a bitrate-spiking keyframe.
    IntraRefresh,
}

impl FrameKind {
    const fn to_u8(self) -> u8 {
        match self {
            Self::Delta => 0,
            Self::Key => 1,
            Self::IntraRefresh => 2,
        }
    }

    fn from_u8(v: u8) -> Result<Self, WireError> {
        match v {
            0 => Ok(Self::Delta),
            1 => Ok(Self::Key),
            2 => Ok(Self::IntraRefresh),
            other => Err(WireError::Unknown {
                what: "frame kind",
                value: u32::from(other),
            }),
        }
    }
}

/// Bit flags describing a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameFlags(pub u16);

impl FrameFlags {
    /// Encoder used screen-content tools (palette/IBC/text-tuned QP).
    pub const SCREEN_CONTENT: Self = Self(1 << 0);
    /// Frame is HDR (otherwise SDR).
    pub const HDR: Self = Self(1 << 1);
    /// The hardware cursor is composited into the picture (no separate cursor channel).
    pub const CURSOR_EMBEDDED: Self = Self(1 << 2);
    /// Sent in response to a controller keyframe/refresh request.
    pub const REQUESTED: Self = Self(1 << 3);

    /// True when every bit of `other` is set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Returns `self` with the bits of `other` set.
    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// A changed region of the picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Left edge.
    pub x: u16,
    /// Top edge.
    pub y: u16,
    /// Width.
    pub w: u16,
    /// Height.
    pub h: u16,
}

/// Parsed video frame header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFrameHeader {
    /// Bitstream format of the payload.
    pub codec: VideoCodec,
    /// Dependency class.
    pub kind: FrameKind,
    /// Which monitor this frame belongs to.
    pub monitor: u8,
    /// Frame flags.
    pub flags: FrameFlags,
    /// Picture width in pixels.
    pub width: u16,
    /// Picture height in pixels.
    pub height: u16,
    /// Monotonic frame counter (wraps).
    pub frame_seq: u32,
    /// The frame this one predicts from.
    pub ref_seq: u32,
    /// Length of the codec payload that follows the header.
    pub payload_len: u32,
    /// Sender clock when the frame was captured.
    pub capture_us: u64,
    /// Sender clock when the encoded frame was handed to the transport.
    pub send_us: u64,
    /// Regions that changed since the previous frame (empty = unknown / whole frame).
    pub dirty: Vec<Rect>,
}

impl VideoFrameHeader {
    /// Total encoded size of this header.
    #[must_use]
    pub fn wire_len(&self) -> usize {
        FIXED_LEN + RECT_LEN * self.dirty.len()
    }

    /// Appends the header to `out`.
    pub fn encode(&self, out: &mut impl BufMut) -> Result<(), WireError> {
        if self.dirty.len() > MAX_DIRTY_RECTS {
            return Err(WireError::TooLarge {
                what: "dirty rect count",
                len: self.dirty.len(),
                max: MAX_DIRTY_RECTS,
            });
        }
        if self.payload_len > MAX_PAYLOAD {
            return Err(WireError::TooLarge {
                what: "video payload",
                len: self.payload_len as usize,
                max: MAX_PAYLOAD as usize,
            });
        }
        let codec = u8::try_from(self.codec as i32).map_err(|_| WireError::Invalid("codec"))?;
        out.put_u8(WIRE_VERSION);
        out.put_u8(codec);
        out.put_u8(self.kind.to_u8());
        out.put_u8(self.monitor);
        out.put_u16_le(self.flags.0);
        out.put_u16_le(self.width);
        out.put_u16_le(self.height);
        out.put_u16_le(
            u16::try_from(self.dirty.len()).map_err(|_| WireError::TooLarge {
                what: "dirty rect count",
                len: self.dirty.len(),
                max: MAX_DIRTY_RECTS,
            })?,
        );
        out.put_u32_le(self.frame_seq);
        out.put_u32_le(self.ref_seq);
        out.put_u32_le(self.payload_len);
        out.put_u64_le(self.capture_us);
        out.put_u64_le(self.send_us);
        for r in &self.dirty {
            out.put_u16_le(r.x);
            out.put_u16_le(r.y);
            out.put_u16_le(r.w);
            out.put_u16_le(r.h);
        }
        Ok(())
    }

    /// Given the first [`FIXED_LEN`] bytes of a header, returns how many *more* header bytes
    /// (the dirty rectangles) must be read before [`decode`](Self::decode) can be called.
    pub fn extra_len(fixed: &[u8; FIXED_LEN]) -> Result<usize, WireError> {
        if fixed[0] != WIRE_VERSION {
            return Err(WireError::BadVersion(fixed[0]));
        }
        let count = usize::from(u16::from_le_bytes([fixed[10], fixed[11]]));
        if count > MAX_DIRTY_RECTS {
            return Err(WireError::TooLarge {
                what: "dirty rect count",
                len: count,
                max: MAX_DIRTY_RECTS,
            });
        }
        Ok(count * RECT_LEN)
    }

    /// Decodes a complete header (`FIXED_LEN + extra_len` bytes, no more, no less).
    pub fn decode(buf: &[u8]) -> Result<Self, WireError> {
        let mut r = Reader::new(buf);
        let version = r.u8()?;
        if version != WIRE_VERSION {
            return Err(WireError::BadVersion(version));
        }
        let codec_raw = r.u8()?;
        let codec = VideoCodec::try_from(i32::from(codec_raw))
            .ok()
            .filter(|c| *c != VideoCodec::Unspecified)
            .ok_or(WireError::Unknown {
                what: "video codec",
                value: u32::from(codec_raw),
            })?;
        let kind = FrameKind::from_u8(r.u8()?)?;
        let monitor = r.u8()?;
        let flags = FrameFlags(r.u16()?);
        let width = r.u16()?;
        let height = r.u16()?;
        if width == 0 || height == 0 {
            return Err(WireError::Invalid("frame dimensions"));
        }
        let dirty_count = usize::from(r.u16()?);
        if dirty_count > MAX_DIRTY_RECTS {
            return Err(WireError::TooLarge {
                what: "dirty rect count",
                len: dirty_count,
                max: MAX_DIRTY_RECTS,
            });
        }
        let frame_seq = r.u32()?;
        let ref_seq = r.u32()?;
        let payload_len = r.u32()?;
        if payload_len > MAX_PAYLOAD {
            return Err(WireError::TooLarge {
                what: "video payload",
                len: payload_len as usize,
                max: MAX_PAYLOAD as usize,
            });
        }
        let capture_us = r.u64()?;
        let send_us = r.u64()?;
        let mut dirty = Vec::with_capacity(dirty_count);
        for _ in 0..dirty_count {
            dirty.push(Rect {
                x: r.u16()?,
                y: r.u16()?,
                w: r.u16()?,
                h: r.u16()?,
            });
        }
        r.finish()?;
        Ok(Self {
            codec,
            kind,
            monitor,
            flags,
            width,
            height,
            frame_seq,
            ref_seq,
            payload_len,
            capture_us,
            send_us,
            dirty,
        })
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn sample() -> VideoFrameHeader {
        VideoFrameHeader {
            codec: VideoCodec::H264,
            kind: FrameKind::Key,
            monitor: 1,
            flags: FrameFlags::SCREEN_CONTENT.with(FrameFlags::REQUESTED),
            width: 3840,
            height: 2160,
            frame_seq: 42,
            ref_seq: 42,
            payload_len: 123_456,
            capture_us: 1_000_000,
            send_us: 1_004_500,
            dirty: vec![
                Rect {
                    x: 0,
                    y: 0,
                    w: 100,
                    h: 50,
                },
                Rect {
                    x: 10,
                    y: 20,
                    w: 30,
                    h: 40,
                },
            ],
        }
    }

    #[test]
    fn roundtrip_and_length() {
        let h = sample();
        let mut buf = Vec::new();
        h.encode(&mut buf).unwrap();
        assert_eq!(buf.len(), h.wire_len());
        assert_eq!(buf.len(), FIXED_LEN + 2 * RECT_LEN);
        assert_eq!(VideoFrameHeader::decode(&buf).unwrap(), h);
    }

    #[test]
    fn streaming_read_uses_extra_len() {
        let h = sample();
        let mut buf = Vec::new();
        h.encode(&mut buf).unwrap();
        let mut fixed = [0u8; FIXED_LEN];
        fixed.copy_from_slice(&buf[..FIXED_LEN]);
        assert_eq!(VideoFrameHeader::extra_len(&fixed).unwrap(), 2 * RECT_LEN);
    }

    #[test]
    fn flags_compose() {
        let f = FrameFlags::SCREEN_CONTENT.with(FrameFlags::HDR);
        assert!(f.contains(FrameFlags::HDR) && f.contains(FrameFlags::SCREEN_CONTENT));
        assert!(!f.contains(FrameFlags::REQUESTED));
    }

    #[test]
    fn rejects_bad_version_codec_kind_and_sizes() {
        let mut buf = Vec::new();
        sample().encode(&mut buf).unwrap();

        let mut bad = buf.clone();
        bad[0] = 9;
        assert_eq!(
            VideoFrameHeader::decode(&bad),
            Err(WireError::BadVersion(9))
        );

        let mut bad = buf.clone();
        bad[1] = 0; // Unspecified codec
        assert!(matches!(
            VideoFrameHeader::decode(&bad),
            Err(WireError::Unknown { .. })
        ));

        let mut bad = buf.clone();
        bad[2] = 77;
        assert!(matches!(
            VideoFrameHeader::decode(&bad),
            Err(WireError::Unknown { .. })
        ));

        let mut bad = buf.clone();
        bad[6] = 0;
        bad[7] = 0; // width = 0
        assert_eq!(
            VideoFrameHeader::decode(&bad),
            Err(WireError::Invalid("frame dimensions"))
        );

        let mut bad = buf.clone();
        bad[20..24].copy_from_slice(&(MAX_PAYLOAD + 1).to_le_bytes());
        assert!(matches!(
            VideoFrameHeader::decode(&bad),
            Err(WireError::TooLarge { .. })
        ));

        let mut bad = buf;
        bad[10..12].copy_from_slice(&65u16.to_le_bytes());
        assert!(matches!(
            VideoFrameHeader::decode(&bad),
            Err(WireError::TooLarge { .. })
        ));
    }

    #[test]
    fn truncated_and_trailing_input_is_rejected() {
        let mut buf = Vec::new();
        sample().encode(&mut buf).unwrap();
        assert!(matches!(
            VideoFrameHeader::decode(&buf[..buf.len() - 1]),
            Err(WireError::Truncated { .. })
        ));
        buf.push(0);
        assert_eq!(VideoFrameHeader::decode(&buf), Err(WireError::Trailing(1)));
    }

    #[test]
    fn encode_rejects_too_many_rects() {
        let mut h = sample();
        h.dirty = vec![
            Rect {
                x: 0,
                y: 0,
                w: 1,
                h: 1
            };
            MAX_DIRTY_RECTS + 1
        ];
        assert!(matches!(
            h.encode(&mut Vec::new()),
            Err(WireError::TooLarge { .. })
        ));
    }

    proptest! {
        #[test]
        fn decode_never_panics(data in proptest::collection::vec(any::<u8>(), 0..200)) {
            let _ = VideoFrameHeader::decode(&data);
        }

        #[test]
        fn roundtrip_arbitrary(
            kind in 0u8..3, monitor in any::<u8>(), flags in any::<u16>(),
            width in 1u16.., height in 1u16.., seq in any::<u32>(), rseq in any::<u32>(),
            payload in 0u32..=MAX_PAYLOAD, cap in any::<u64>(), send in any::<u64>(),
            rects in proptest::collection::vec((any::<u16>(), any::<u16>(), any::<u16>(), any::<u16>()), 0..=MAX_DIRTY_RECTS),
        ) {
            let h = VideoFrameHeader {
                codec: VideoCodec::Hevc,
                kind: FrameKind::from_u8(kind).unwrap(),
                monitor, flags: FrameFlags(flags), width, height,
                frame_seq: seq, ref_seq: rseq, payload_len: payload,
                capture_us: cap, send_us: send,
                dirty: rects.into_iter().map(|(x, y, w, h)| Rect { x, y, w, h }).collect(),
            };
            let mut buf = Vec::new();
            h.encode(&mut buf).unwrap();
            prop_assert_eq!(VideoFrameHeader::decode(&buf).unwrap(), h);
        }
    }
}
