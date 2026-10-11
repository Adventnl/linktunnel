//! Cursor position updates (unreliable datagrams, latest wins).
//!
//! The controller draws the remote cursor itself, immediately, from these updates, so pointer
//! motion feels instant regardless of video latency (spec §2.1). Cursor *shape* bitmaps are rare
//! and large, so they travel on the reliable control stream keyed by `shape_id`.
//!
//! ```text
//!  off  size  field
//!    0     1  wire version (1)
//!    1     1  flags (bit0 visible, bit1 shape changed)
//!    2     1  monitor index
//!    3     1  reserved (0)
//!    4     4  shape_id
//!    8     4  x (i32, monitor pixels, may be negative on multi-monitor layouts)
//!   12     4  y
//!   16     4  seq (wraps)
//!   20     8  ts_us (sender monotonic clock)
//! ```

use bytes::BufMut;

use super::{Reader, WIRE_VERSION, WireError};

/// Encoded size.
pub const LEN: usize = 28;

const FLAG_VISIBLE: u8 = 1;
const FLAG_SHAPE_CHANGED: u8 = 2;

/// A cursor position/visibility sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorUpdate {
    /// Cursor is shown (hidden while typing, in games, etc.).
    pub visible: bool,
    /// `shape_id` differs from the previous update, so the controller should look it up.
    pub shape_changed: bool,
    /// Monitor the cursor is on.
    pub monitor: u8,
    /// Which cursor bitmap to draw.
    pub shape_id: u32,
    /// Hot-spot x in monitor pixels.
    pub x: i32,
    /// Hot-spot y in monitor pixels.
    pub y: i32,
    /// Counter; receiver keeps only the highest (serial arithmetic).
    pub seq: u32,
    /// Sender clock when sampled.
    pub ts_us: u64,
}

impl CursorUpdate {
    /// Appends the encoded update to `out`.
    pub fn encode(&self, out: &mut impl BufMut) {
        let mut flags = 0u8;
        if self.visible {
            flags |= FLAG_VISIBLE;
        }
        if self.shape_changed {
            flags |= FLAG_SHAPE_CHANGED;
        }
        out.put_u8(WIRE_VERSION);
        out.put_u8(flags);
        out.put_u8(self.monitor);
        out.put_u8(0);
        out.put_u32_le(self.shape_id);
        out.put_i32_le(self.x);
        out.put_i32_le(self.y);
        out.put_u32_le(self.seq);
        out.put_u64_le(self.ts_us);
    }

    /// Decodes exactly one update.
    pub fn decode(buf: &[u8]) -> Result<Self, WireError> {
        let mut r = Reader::new(buf);
        let version = r.u8()?;
        if version != WIRE_VERSION {
            return Err(WireError::BadVersion(version));
        }
        let flags = r.u8()?;
        let monitor = r.u8()?;
        let _reserved = r.u8()?;
        let update = Self {
            visible: flags & FLAG_VISIBLE != 0,
            shape_changed: flags & FLAG_SHAPE_CHANGED != 0,
            monitor,
            shape_id: r.u32()?,
            x: r.i32()?,
            y: r.i32()?,
            seq: r.u32()?,
            ts_us: r.u64()?,
        };
        r.finish()?;
        Ok(update)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn roundtrip_with_negative_coordinates() {
        let u = CursorUpdate {
            visible: true,
            shape_changed: true,
            monitor: 2,
            shape_id: 7,
            x: -1920,
            y: 1079,
            seq: u32::MAX,
            ts_us: 123_456_789,
        };
        let mut buf = Vec::new();
        u.encode(&mut buf);
        assert_eq!(buf.len(), LEN);
        assert_eq!(CursorUpdate::decode(&buf).unwrap(), u);
    }

    #[test]
    fn rejects_wrong_sizes_and_versions() {
        let mut buf = Vec::new();
        CursorUpdate {
            visible: false,
            shape_changed: false,
            monitor: 0,
            shape_id: 0,
            x: 0,
            y: 0,
            seq: 0,
            ts_us: 0,
        }
        .encode(&mut buf);
        assert!(matches!(
            CursorUpdate::decode(&buf[..LEN - 1]),
            Err(WireError::Truncated { .. })
        ));
        let mut long = buf.clone();
        long.push(0);
        assert_eq!(CursorUpdate::decode(&long), Err(WireError::Trailing(1)));
        buf[0] = 0;
        assert_eq!(CursorUpdate::decode(&buf), Err(WireError::BadVersion(0)));
    }

    proptest! {
        #[test]
        fn decode_never_panics(data in proptest::collection::vec(any::<u8>(), 0..64)) {
            let _ = CursorUpdate::decode(&data);
        }

        #[test]
        fn roundtrip_arbitrary(
            visible in any::<bool>(), shape_changed in any::<bool>(), monitor in any::<u8>(),
            shape_id in any::<u32>(), x in any::<i32>(), y in any::<i32>(),
            seq in any::<u32>(), ts_us in any::<u64>(),
        ) {
            let u = CursorUpdate { visible, shape_changed, monitor, shape_id, x, y, seq, ts_us };
            let mut buf = Vec::new();
            u.encode(&mut buf);
            prop_assert_eq!(CursorUpdate::decode(&buf).unwrap(), u);
        }
    }
}
