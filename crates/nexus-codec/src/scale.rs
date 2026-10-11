//! Area-average downscaling of BGRA frames (the resolution rung of the quality ladder).
//!
//! Averaging every source pixel that falls into a destination pixel keeps text legible when
//! shrinking (nearest-neighbour shimmers, bilinear blurs thin strokes). Integer arithmetic only.

use bytes::Bytes;

use crate::CodecError;
use crate::types::{Frame, FrameFormat, Rect};

/// Scales `frame` to `width`×`height` (each ≤ the source size). The dirty rectangles are scaled
/// along with the pixels (rounded outwards, so they still cover the changed area).
pub fn downscale_bgra(frame: &Frame, width: u32, height: u32) -> Result<Frame, CodecError> {
    frame.validate()?;
    if width == 0 || height == 0 || width > frame.width || height > frame.height {
        return Err(CodecError::Invalid(format!(
            "cannot scale {}x{} to {width}x{height}",
            frame.width, frame.height
        )));
    }
    if (width, height) == (frame.width, frame.height) {
        return Ok(frame.clone());
    }
    let (sw, sh, dw, dh) = (
        frame.width as usize,
        frame.height as usize,
        width as usize,
        height as usize,
    );
    // Source column/row boundaries for every destination index.
    let xs: Vec<usize> = (0..=dw).map(|x| x * sw / dw).collect();
    let ys: Vec<usize> = (0..=dh).map(|y| y * sh / dh).collect();
    let mut out = vec![0u8; dw * dh * 4];
    for dy in 0..dh {
        let (y0, y1) = (ys[dy], ys[dy + 1].max(ys[dy] + 1));
        for dx in 0..dw {
            let (x0, x1) = (xs[dx], xs[dx + 1].max(xs[dx] + 1));
            let mut acc = [0u32; 3];
            for y in y0..y1 {
                let row = &frame.data[y * frame.stride..];
                for x in x0..x1 {
                    let p = &row[x * 4..x * 4 + 3];
                    acc[0] += u32::from(p[0]);
                    acc[1] += u32::from(p[1]);
                    acc[2] += u32::from(p[2]);
                }
            }
            #[allow(clippy::cast_possible_truncation)]
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            let o = (dy * dw + dx) * 4;
            // Rounded average.
            out[o] = u8::try_from((acc[0] + n / 2) / n).unwrap_or(255);
            out[o + 1] = u8::try_from((acc[1] + n / 2) / n).unwrap_or(255);
            out[o + 2] = u8::try_from((acc[2] + n / 2) / n).unwrap_or(255);
            out[o + 3] = 255;
        }
    }
    let scale_rect = |r: &Rect| {
        let fx = |v: u16, up: bool| {
            let n = u32::from(v) * width;
            let q = if up {
                n.div_ceil(frame.width)
            } else {
                n / frame.width
            };
            u16::try_from(q.min(width)).unwrap_or(u16::MAX)
        };
        let fy = |v: u16, up: bool| {
            let n = u32::from(v) * height;
            let q = if up {
                n.div_ceil(frame.height)
            } else {
                n / frame.height
            };
            u16::try_from(q.min(height)).unwrap_or(u16::MAX)
        };
        let (x0, y0) = (fx(r.x, false), fy(r.y, false));
        let (x1, y1) = (
            fx(r.x.saturating_add(r.w), true),
            fy(r.y.saturating_add(r.h), true),
        );
        Rect {
            x: x0,
            y: y0,
            w: x1.saturating_sub(x0).max(1),
            h: y1.saturating_sub(y0).max(1),
        }
    };
    Ok(Frame {
        width,
        height,
        stride: dw * 4,
        format: FrameFormat::Bgra8,
        data: Bytes::from(out),
        captured_us: frame.captured_us,
        dirty: frame.dirty.iter().map(scale_rect).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checker(w: u32, h: u32) -> Frame {
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let v = if (x + y) % 2 == 0 { 255 } else { 0 };
                data.extend_from_slice(&[v, v, v, 255]);
            }
        }
        Frame {
            width: w,
            height: h,
            stride: (w * 4) as usize,
            format: FrameFormat::Bgra8,
            data: Bytes::from(data),
            captured_us: 9,
            dirty: vec![Rect {
                x: 10,
                y: 10,
                w: 20,
                h: 20,
            }],
        }
    }

    #[test]
    fn halving_a_checkerboard_gives_mid_grey() {
        let out = downscale_bgra(&checker(64, 64), 32, 32).unwrap();
        assert_eq!((out.width, out.height, out.stride), (32, 32, 128));
        assert!(
            out.data
                .chunks_exact(4)
                .all(|p| (126..=129).contains(&p[0])),
            "area averaging of a checkerboard is grey"
        );
        assert_eq!(out.captured_us, 9);
    }

    #[test]
    fn solid_colours_are_preserved_exactly_at_any_ratio() {
        let mut f = checker(100, 60);
        f.data = Bytes::from(vec![[10u8, 200, 30, 255]; 100 * 60].concat());
        for (w, h) in [(50, 30), (75, 45), (33, 21), (1, 1)] {
            let out = downscale_bgra(&f, w, h).unwrap();
            assert!(
                out.data.chunks_exact(4).all(|p| p == [10, 200, 30, 255]),
                "{w}x{h}"
            );
        }
    }

    #[test]
    fn dirty_rects_still_cover_the_changed_area() {
        let out = downscale_bgra(&checker(100, 100), 33, 33).unwrap();
        let r = out.dirty[0];
        // Source area x 10..30 maps to 3.3..9.9 → must cover 3..10.
        assert!(r.x <= 3 && r.x + r.w >= 10, "{r:?}");
    }

    #[test]
    fn identity_and_invalid_sizes() {
        let f = checker(16, 16);
        assert_eq!(downscale_bgra(&f, 16, 16).unwrap().data, f.data);
        assert!(
            downscale_bgra(&f, 32, 32).is_err(),
            "upscaling is not supported"
        );
        assert!(downscale_bgra(&f, 0, 4).is_err());
    }
}
