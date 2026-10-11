use nexus_proto::wire::input::ScrollUnit;

/// Turns scroll deltas into whole wheel notches, carrying the remainder.
///
/// X11's core protocol (and plain wheel mice) only know discrete notches, so smooth trackpad
/// scrolling (pixels) and fractional detents are accumulated until they add up to a notch.
#[derive(Debug, Clone)]
pub struct ScrollAccumulator {
    pixels_per_notch: i32,
    x: i32,
    y: i32,
}

/// One notch in `Detent` units.
pub const DETENT: i32 = 120;

impl ScrollAccumulator {
    /// `pixels_per_notch` is how many pixels of trackpad travel equal one wheel click.
    #[must_use]
    pub fn new(pixels_per_notch: i32) -> Self {
        Self {
            pixels_per_notch: pixels_per_notch.max(1),
            x: 0,
            y: 0,
        }
    }

    /// Adds a delta and returns the whole notches to perform now: `(horizontal, vertical)`,
    /// positive = right / up.
    pub fn push(&mut self, dx: i32, dy: i32, unit: ScrollUnit) -> (i32, i32) {
        let per = match unit {
            ScrollUnit::Detent => DETENT,
            ScrollUnit::Pixel => self.pixels_per_notch,
        };
        self.x = self.x.saturating_add(dx);
        self.y = self.y.saturating_add(dy);
        let nx = self.x / per;
        let ny = self.y / per;
        self.x -= nx * per;
        self.y -= ny * per;
        (nx, ny)
    }

    /// Forgets any partial notch (call at the end of a gesture).
    pub fn reset(&mut self) {
        self.x = 0;
        self.y = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detents_pass_through() {
        let mut a = ScrollAccumulator::new(40);
        assert_eq!(a.push(0, 120, ScrollUnit::Detent), (0, 1));
        assert_eq!(a.push(0, -240, ScrollUnit::Detent), (0, -2));
        assert_eq!(a.push(120, 0, ScrollUnit::Detent), (1, 0));
    }

    #[test]
    fn pixels_accumulate_and_remainders_carry() {
        let mut a = ScrollAccumulator::new(40);
        let total: i32 = (0..10).map(|_| a.push(0, 17, ScrollUnit::Pixel).1).sum();
        assert_eq!(
            total, 4,
            "170 px at 40 px/notch is 4 notches with 10 px left over"
        );
        assert_eq!(a.push(0, 30, ScrollUnit::Pixel), (0, 1));
    }

    #[test]
    fn direction_changes_cancel_the_partial_notch() {
        let mut a = ScrollAccumulator::new(40);
        assert_eq!(a.push(0, 30, ScrollUnit::Pixel), (0, 0));
        assert_eq!(a.push(0, -30, ScrollUnit::Pixel), (0, 0));
        a.reset();
        assert_eq!(a.push(0, 39, ScrollUnit::Pixel), (0, 0));
    }

    #[test]
    fn saturates_instead_of_overflowing() {
        let mut a = ScrollAccumulator::new(1);
        a.push(i32::MAX, i32::MAX, ScrollUnit::Pixel);
        a.push(i32::MAX, i32::MAX, ScrollUnit::Pixel);
    }
}
