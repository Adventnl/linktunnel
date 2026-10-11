//! Where the remote picture sits inside the window, and coordinate mapping both ways.

/// A rectangle in window pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Left.
    pub x: u32,
    /// Top.
    pub y: u32,
    /// Width.
    pub w: u32,
    /// Height.
    pub h: u32,
}

/// How the picture is scaled to the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleMode {
    /// Largest size that fits, aspect ratio preserved, letterboxed.
    Fit,
    /// One picture pixel per window pixel (centred; cropped if the window is smaller).
    Pixel,
}

/// The destination rectangle for a `pic`-sized picture in a `win`-sized window.
#[must_use]
pub fn dest_rect(pic: (u32, u32), win: (u32, u32), mode: ScaleMode) -> Rect {
    let (pw, ph) = (pic.0.max(1), pic.1.max(1));
    let (ww, wh) = (win.0.max(1), win.1.max(1));
    let (w, h) = match mode {
        ScaleMode::Pixel => (pw, ph),
        ScaleMode::Fit => {
            // Compare ww/pw with wh/ph without floating point.
            if u64::from(ww) * u64::from(ph) <= u64::from(wh) * u64::from(pw) {
                (
                    ww,
                    u32::try_from(u64::from(ph) * u64::from(ww) / u64::from(pw))
                        .unwrap_or(wh)
                        .max(1),
                )
            } else {
                (
                    u32::try_from(u64::from(pw) * u64::from(wh) / u64::from(ph))
                        .unwrap_or(ww)
                        .max(1),
                    wh,
                )
            }
        }
    };
    Rect {
        x: ww.saturating_sub(w) / 2,
        y: wh.saturating_sub(h) / 2,
        w,
        h,
    }
}

/// Maps a window position to a picture pixel, or `None` if it is in the letterbox bars.
#[must_use]
pub fn window_to_picture(pos: (f64, f64), pic: (u32, u32), dest: Rect) -> Option<(i32, i32)> {
    let (x, y) = (pos.0 - f64::from(dest.x), pos.1 - f64::from(dest.y));
    if x < 0.0 || y < 0.0 || x >= f64::from(dest.w) || y >= f64::from(dest.h) {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)]
    Some((
        (x * f64::from(pic.0) / f64::from(dest.w)).floor() as i32,
        (y * f64::from(pic.1) / f64::from(dest.h)).floor() as i32,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_letterboxes_and_centres() {
        // 16:9 picture in a square window: full width, bars top and bottom.
        assert_eq!(
            dest_rect((1920, 1080), (800, 800), ScaleMode::Fit),
            Rect {
                x: 0,
                y: 175,
                w: 800,
                h: 450
            }
        );
        // 16:9 picture in a tall window of the same width gives identical scale.
        assert_eq!(
            dest_rect((1920, 1080), (1920, 1080), ScaleMode::Fit),
            Rect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080
            }
        );
        // Wide window: pillarbox.
        assert_eq!(
            dest_rect((1000, 1000), (1600, 1000), ScaleMode::Fit),
            Rect {
                x: 300,
                y: 0,
                w: 1000,
                h: 1000
            }
        );
        assert_eq!(
            dest_rect((100, 100), (50, 50), ScaleMode::Pixel),
            Rect {
                x: 0,
                y: 0,
                w: 100,
                h: 100
            }
        );
        // Degenerate sizes never divide by zero.
        let _ = dest_rect((0, 0), (0, 0), ScaleMode::Fit);
    }

    #[test]
    fn mapping_roundtrip_and_bars() {
        let pic = (1920, 1080);
        let dest = dest_rect(pic, (960, 800), ScaleMode::Fit); // 960x540 at y=130
        assert_eq!(window_to_picture((0.0, 0.0), pic, dest), None, "top bar");
        assert_eq!(
            window_to_picture((480.0, 400.0), pic, dest),
            Some((960, 540))
        );
        assert_eq!(
            window_to_picture((959.9, 669.9), pic, dest),
            Some((1919, 1079))
        );
        assert_eq!(window_to_picture((960.0, 400.0), pic, dest), None);
    }
}
