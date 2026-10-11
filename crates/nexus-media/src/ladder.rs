//! The quality ladder: given a bitrate budget, decide frame rate and resolution.
//!
//! The rate controller says *how many bits per second* the path can carry. What to spend them on
//! is a separate decision with a clear preference order (spec §5.4):
//!
//! * **text / UI content** (small changes, mostly static): keep full resolution so text stays
//!   crisp and give up frame rate first: nobody can tell 30 from 60 fps on a code editor;
//! * **video content** (large changed areas, e.g. a playing movie): keep the frame rate and give
//!   up resolution first: motion matters more than pixel sharpness there.
//!
//! The ladder keeps *bits per pixel per frame* above a floor below which H.264 screen content
//! turns to mush, and uses hysteresis so a bitrate hovering at a threshold does not flap.

/// What the screen is mostly showing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Content {
    /// Small, sparse changes (typing, scrolling a document).
    Text,
    /// Large areas changing every frame (video, games, animations).
    Video,
}

/// Decides [`Content`] from the fraction of the screen that changed per frame.
#[derive(Debug, Clone)]
pub struct ContentClassifier {
    ewma: f64,
}

impl Default for ContentClassifier {
    fn default() -> Self {
        Self { ewma: 0.0 }
    }
}

impl ContentClassifier {
    /// Records a frame whose damage covered `dirty_fraction` (0.0–1.0) of the screen.
    pub fn frame(&mut self, dirty_fraction: f64) {
        self.ewma = 0.9 * self.ewma + 0.1 * dirty_fraction.clamp(0.0, 1.0);
    }

    /// The current classification; the thresholds differ for entering and leaving `Video` so it
    /// does not flicker.
    #[must_use]
    pub fn class(&self, previous: Content) -> Content {
        match previous {
            Content::Text if self.ewma > 0.35 => Content::Video,
            Content::Video if self.ewma < 0.15 => Content::Text,
            other => other,
        }
    }

    /// Smoothed fraction of the screen changing per frame.
    #[must_use]
    pub fn level(&self) -> f64 {
        self.ewma
    }
}

/// Frame rates the ladder steps through.
pub const FPS_STEPS: [u32; 5] = [60, 45, 30, 24, 15];
/// Resolution scales (percent of native) the ladder steps through.
pub const SCALE_STEPS: [u32; 4] = [100, 75, 50, 35];

/// Below this many bits per pixel per frame, quality degrades visibly: step down.
const BPP_DOWN: f64 = 0.035;
/// Above this, there is room to step back up (hysteresis gap avoids flapping).
const BPP_UP: f64 = 0.075;

/// One rung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rung {
    /// Frames per second.
    pub fps: u32,
    /// Resolution in percent of native.
    pub scale_percent: u32,
}

/// Chooses rungs.
#[derive(Debug, Clone)]
pub struct QualityLadder {
    native: (u32, u32),
    max_fps: u32,
    current: Rung,
    content: Content,
}

impl QualityLadder {
    /// A ladder for a `native`-sized screen that may run up to `max_fps`.
    #[must_use]
    pub fn new(native: (u32, u32), max_fps: u32) -> Self {
        let top = FPS_STEPS
            .iter()
            .copied()
            .find(|f| *f <= max_fps)
            .unwrap_or(FPS_STEPS[FPS_STEPS.len() - 1]);
        Self {
            native,
            max_fps: top,
            current: Rung {
                fps: top,
                scale_percent: 100,
            },
            content: Content::Text,
        }
    }

    /// The highest frame rate this ladder will use.
    #[must_use]
    pub fn max_fps(&self) -> u32 {
        self.max_fps
    }

    /// The rung in use.
    #[must_use]
    pub fn current(&self) -> Rung {
        self.current
    }

    /// Encoded size at a scale (even dimensions, as H.264 requires).
    #[must_use]
    pub fn size_at(&self, scale_percent: u32) -> (u32, u32) {
        let f = |v: u32| {
            u32::try_from((u64::from(v) * u64::from(scale_percent) / 100).max(2))
                .unwrap_or(u32::MAX)
                & !1
        };
        (f(self.native.0), f(self.native.1))
    }

    fn bpp(&self, bitrate_bps: u32, rung: Rung) -> f64 {
        let (w, h) = self.size_at(rung.scale_percent);
        f64::from(bitrate_bps) / (f64::from(w) * f64::from(h) * f64::from(rung.fps))
    }

    fn lower(&self, r: Rung, prefer_fps: bool) -> Option<Rung> {
        let next_fps = FPS_STEPS.iter().copied().find(|f| *f < r.fps);
        let next_scale = SCALE_STEPS.iter().copied().find(|s| *s < r.scale_percent);
        match (prefer_fps, next_fps, next_scale) {
            (true, Some(f), _) | (false, Some(f), None) => Some(Rung { fps: f, ..r }),
            (_, _, Some(s)) => Some(Rung {
                scale_percent: s,
                ..r
            }),
            (_, None, None) => None,
        }
    }

    fn higher(&self, r: Rung, prefer_fps: bool) -> Option<Rung> {
        let up_fps = FPS_STEPS
            .iter()
            .rev()
            .copied()
            .find(|f| *f > r.fps && *f <= self.max_fps);
        let up_scale = SCALE_STEPS
            .iter()
            .rev()
            .copied()
            .find(|s| *s > r.scale_percent);
        // Restore in the reverse order of the preference for giving things up: whatever matters
        // most for this content comes back first.
        match (prefer_fps, up_fps, up_scale) {
            (true, _, Some(s)) => Some(Rung {
                scale_percent: s,
                ..r
            }),
            (true, Some(f), None) => Some(Rung { fps: f, ..r }),
            (false, Some(f), _) => Some(Rung { fps: f, ..r }),
            (false, None, Some(s)) => Some(Rung {
                scale_percent: s,
                ..r
            }),
            (_, None, None) => None,
        }
    }

    /// Updates the plan for a new bitrate and content class; returns the rung if it changed.
    pub fn plan(&mut self, bitrate_bps: u32, content: Content) -> Option<Rung> {
        self.content = content;
        let prefer_fps = content == Content::Text;
        let before = self.current;
        let mut rung = self.current;
        while self.bpp(bitrate_bps, rung) < BPP_DOWN {
            match self.lower(rung, prefer_fps) {
                Some(next) => rung = next,
                None => break,
            }
        }
        // Only step up if the *next* rung would still be comfortably above the floor.
        while let Some(next) = self.higher(rung, prefer_fps) {
            if self.bpp(bitrate_bps, next) >= BPP_UP {
                rung = next;
            } else {
                break;
            }
        }
        self.current = rung;
        (rung != before).then_some(rung)
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
mod tests {
    use super::*;

    #[test]
    fn plenty_of_bandwidth_means_full_quality() {
        let mut l = QualityLadder::new((1920, 1080), 60);
        assert_eq!(
            l.plan(30_000_000, Content::Text),
            None,
            "starts at the top rung and stays"
        );
        assert_eq!(
            l.current(),
            Rung {
                fps: 60,
                scale_percent: 100
            }
        );
    }

    #[test]
    fn text_gives_up_frame_rate_before_resolution() {
        let mut l = QualityLadder::new((1920, 1080), 60);
        // 2.5 Mbit/s at 1080p60 is 0.020 bpp: too thin. For text, fps drops, resolution holds.
        let r = l.plan(2_500_000, Content::Text).unwrap();
        assert_eq!(r.scale_percent, 100, "{r:?}");
        assert!(r.fps < 60, "{r:?}");
        // Pathologically thin: only then does resolution fall.
        let r = l.plan(300_000, Content::Text).unwrap();
        assert!(r.scale_percent < 100 && r.fps == 15, "{r:?}");
    }

    #[test]
    fn video_gives_up_resolution_before_frame_rate() {
        let mut l = QualityLadder::new((1920, 1080), 60);
        let r = l.plan(2_500_000, Content::Video).unwrap();
        assert_eq!(r.fps, 60, "{r:?}");
        assert!(r.scale_percent < 100, "{r:?}");
    }

    #[test]
    fn recovery_restores_what_matters_most_first_and_has_hysteresis() {
        let mut l = QualityLadder::new((1920, 1080), 60);
        l.plan(300_000, Content::Text);
        let low = l.current();
        assert!(low.fps == 15 && low.scale_percent < 100);
        // A bitrate just above the "down" floor is not enough to climb (hysteresis).
        let bpp_floor_bitrate = (0.04
            * f64::from(l.size_at(low.scale_percent).0)
            * f64::from(l.size_at(low.scale_percent).1)
            * 15.0) as u32;
        assert_eq!(l.plan(bpp_floor_bitrate, Content::Text), None);
        // Plenty again: back to the top, resolution first for text.
        let mut seen = vec![];
        for b in [1_000_000, 3_000_000, 8_000_000, 30_000_000] {
            if let Some(r) = l.plan(b, Content::Text) {
                seen.push(r);
            }
        }
        assert_eq!(
            l.current(),
            Rung {
                fps: 60,
                scale_percent: 100
            },
            "{seen:?}"
        );
        assert!(
            seen.windows(2)
                .all(|w| w[1].fps >= w[0].fps && w[1].scale_percent >= w[0].scale_percent),
            "monotone climb: {seen:?}"
        );
    }

    #[test]
    fn a_bitrate_hovering_at_the_threshold_does_not_flap() {
        let mut l = QualityLadder::new((1920, 1080), 60);
        l.plan(3_000_000, Content::Text);
        let mut changes = 0;
        for i in 0..200u32 {
            let jitter = if i % 2 == 0 { 3_100_000 } else { 2_900_000 };
            if l.plan(jitter, Content::Text).is_some() {
                changes += 1;
            }
        }
        assert!(changes <= 1, "flapped {changes} times");
    }

    #[test]
    fn sizes_are_even_and_scaled() {
        let l = QualityLadder::new((1919, 1079), 60);
        let (w, h) = l.size_at(50);
        assert_eq!((w % 2, h % 2), (0, 0));
        assert!(w < 1000 && h < 600);
        assert_eq!(l.size_at(100), (1918, 1078));
    }

    #[test]
    fn content_classifier_has_hysteresis() {
        let mut c = ContentClassifier::default();
        let mut class = Content::Text;
        for _ in 0..60 {
            c.frame(0.9);
            class = c.class(class);
        }
        assert_eq!(class, Content::Video);
        for _ in 0..10 {
            c.frame(0.25); // between the two thresholds
            class = c.class(class);
        }
        assert_eq!(class, Content::Video, "stays video until clearly quiet");
        for _ in 0..60 {
            c.frame(0.01);
            class = c.class(class);
        }
        assert_eq!(class, Content::Text);
    }

    #[test]
    fn max_fps_is_respected() {
        let mut l = QualityLadder::new((1280, 720), 30);
        assert_eq!(l.current().fps, 30);
        l.plan(50_000_000, Content::Text);
        assert_eq!(l.current().fps, 30);
    }
}
