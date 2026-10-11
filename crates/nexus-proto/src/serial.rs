//! Serial-number arithmetic (RFC 1982) for wrapping sequence counters.
//!
//! Frame, packet and input sequence numbers are `u32`/`u16` counters that eventually wrap.
//! Comparing them with `<` would break at the wrap point, so every comparison goes through
//! these helpers.

/// True when `a` is strictly newer than `b` in `u32` serial arithmetic.
#[must_use]
pub const fn newer_u32(a: u32, b: u32) -> bool {
    a != b && a.wrapping_sub(b) < (1 << 31)
}

/// True when `a` is strictly newer than `b` in `u16` serial arithmetic.
#[must_use]
pub const fn newer_u16(a: u16, b: u16) -> bool {
    a != b && a.wrapping_sub(b) < (1 << 15)
}

/// Forward distance from `from` to `to` (how many increments get `from` to `to`).
#[must_use]
pub const fn distance_u32(from: u32, to: u32) -> u32 {
    to.wrapping_sub(from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_ordering() {
        assert!(newer_u32(5, 4));
        assert!(!newer_u32(4, 5));
        assert!(!newer_u32(7, 7));
        assert!(newer_u16(2, 1));
        assert!(!newer_u16(1, 2));
    }

    #[test]
    fn wraparound_ordering() {
        assert!(newer_u32(0, u32::MAX));
        assert!(newer_u32(3, u32::MAX - 2));
        assert!(!newer_u32(u32::MAX, 0));
        assert!(newer_u16(0, u16::MAX));
        assert!(!newer_u16(u16::MAX, 0));
    }

    #[test]
    fn distance_wraps() {
        assert_eq!(distance_u32(u32::MAX, 1), 2);
        assert_eq!(distance_u32(10, 10), 0);
    }
}
