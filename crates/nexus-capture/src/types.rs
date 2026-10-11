use std::time::Duration;

use bytes::Bytes;
use nexus_codec::Frame;

use crate::CaptureError;

/// One display of the machine being captured.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorInfo {
    /// Stable index used on the wire.
    pub index: u8,
    /// Human-readable name ("DP-1", "Built-in Retina Display").
    pub name: String,
    /// Left edge in the virtual desktop.
    pub x: i32,
    /// Top edge in the virtual desktop.
    pub y: i32,
    /// Width in device pixels.
    pub width: u32,
    /// Height in device pixels.
    pub height: u32,
    /// Device pixels per logical pixel.
    pub scale: f32,
    /// Refresh rate in Hz (0 = unknown).
    pub refresh_hz: f32,
    /// Is the primary display.
    pub primary: bool,
}

/// Produces frames for one monitor. Owned by a dedicated capture thread.
pub trait Capturer: Send {
    /// Backend name for logs.
    fn name(&self) -> &'static str;

    /// The monitor being captured.
    fn monitor(&self) -> &MonitorInfo;

    /// Waits up to `timeout` for the screen to change and returns the new picture, or `None` if
    /// nothing changed in that time. The first call always returns the full picture.
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>, CaptureError>;
}

/// A cursor bitmap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorBitmap {
    /// Backend-assigned identity: equal ids mean equal bitmaps.
    pub shape_id: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Hot-spot x within the bitmap.
    pub hot_x: u32,
    /// Hot-spot y within the bitmap.
    pub hot_y: u32,
    /// Premultiplied BGRA pixels.
    pub bgra: Bytes,
}

/// Where the cursor is and what it looks like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorSample {
    /// Hot-spot x in virtual-desktop pixels.
    pub x: i32,
    /// Hot-spot y in virtual-desktop pixels.
    pub y: i32,
    /// Cursor currently shown.
    pub visible: bool,
    /// Identity of the current shape.
    pub shape_id: u32,
    /// The bitmap, present only the first time a `shape_id` is reported.
    pub new_shape: Option<CursorBitmap>,
}

/// Samples the cursor. Cheap enough to poll at 100+ Hz.
pub trait CursorSource: Send {
    /// Current cursor state.
    fn poll(&mut self) -> Result<CursorSample, CaptureError>;
}

/// A way of capturing this machine's screens.
pub trait CaptureBackend: Send + Sync {
    /// Backend name for logs and the HUD.
    fn name(&self) -> &'static str;

    /// The monitors present right now.
    fn monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError>;

    /// Starts capturing monitor `index`.
    fn open(&self, index: u8) -> Result<Box<dyn Capturer>, CaptureError>;

    /// A cursor sampler, if this backend can provide one.
    fn cursor_source(&self) -> Result<Option<Box<dyn CursorSource>>, CaptureError>;
}

/// Merges `rects` so no more than `max` remain, falling back to their bounding box.
#[must_use]
pub fn limit_rects(mut rects: Vec<nexus_codec::Rect>, max: usize) -> Vec<nexus_codec::Rect> {
    if rects.len() <= max {
        return rects;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (u16::MAX, u16::MAX, 0u16, 0u16);
    for r in rects.drain(..) {
        x0 = x0.min(r.x);
        y0 = y0.min(r.y);
        x1 = x1.max(r.x.saturating_add(r.w));
        y1 = y1.max(r.y.saturating_add(r.h));
    }
    vec![nexus_codec::Rect {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_codec::Rect;

    #[test]
    fn rect_limit_keeps_small_lists_and_boxes_large_ones() {
        let few = vec![Rect {
            x: 1,
            y: 2,
            w: 3,
            h: 4,
        }];
        assert_eq!(limit_rects(few.clone(), 4), few);
        let many: Vec<Rect> = (0..10)
            .map(|i| Rect {
                x: i * 10,
                y: i * 5,
                w: 4,
                h: 4,
            })
            .collect();
        let merged = limit_rects(many, 4);
        assert_eq!(
            merged,
            vec![Rect {
                x: 0,
                y: 0,
                w: 94,
                h: 49
            }]
        );
    }
}
