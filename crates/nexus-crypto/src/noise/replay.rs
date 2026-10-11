//! Sliding-window replay protection (the IPsec/WireGuard scheme).
//!
//! For each sender we remember the highest counter seen and a 64-bit bitmap of which of the
//! preceding 64 counters have arrived. A counter is accepted at most once, out-of-order delivery
//! within the window is fine, and anything older than the window is rejected (we can no longer
//! tell whether it is a replay).

use std::collections::HashMap;

use super::NoiseError;

const WINDOW: u64 = 64;

#[derive(Debug, Clone, Copy)]
struct Window {
    highest: u64,
    /// Bit `i` set ⇔ counter `highest - i` was seen.
    seen: u64,
}

/// Tracks counters per sender key.
#[derive(Debug)]
pub struct ReplayGuard {
    senders: HashMap<[u8; 32], Window>,
    max_senders: usize,
}

impl Default for ReplayGuard {
    fn default() -> Self {
        Self::new(4096)
    }
}

impl ReplayGuard {
    /// Creates a guard remembering at most `max_senders` distinct senders. When full, an
    /// arbitrary old entry is evicted: a forgotten sender merely loses replay history, which the
    /// envelope timestamp window still bounds.
    #[must_use]
    pub fn new(max_senders: usize) -> Self {
        Self {
            senders: HashMap::new(),
            max_senders: max_senders.max(1),
        }
    }

    /// Records `counter` from `sender`, or fails if it is a replay / too old.
    pub fn check_and_record(&mut self, sender: &[u8; 32], counter: u64) -> Result<(), NoiseError> {
        if !self.senders.contains_key(sender)
            && self.senders.len() >= self.max_senders
            && let Some(victim) = self.senders.keys().next().copied()
        {
            self.senders.remove(&victim);
        }
        match self.senders.get_mut(sender) {
            None => {
                self.senders.insert(
                    *sender,
                    Window {
                        highest: counter,
                        seen: 1,
                    },
                );
                Ok(())
            }
            Some(w) if counter > w.highest => {
                let shift = counter - w.highest;
                w.seen = if shift >= WINDOW { 0 } else { w.seen << shift };
                w.seen |= 1;
                w.highest = counter;
                Ok(())
            }
            Some(w) => {
                let age = w.highest - counter;
                if age >= WINDOW {
                    return Err(NoiseError::Replay);
                }
                let bit = 1u64 << age;
                if w.seen & bit != 0 {
                    return Err(NoiseError::Replay);
                }
                w.seen |= bit;
                Ok(())
            }
        }
    }

    /// Number of senders currently tracked.
    #[must_use]
    pub fn tracked_senders(&self) -> usize {
        self.senders.len()
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const A: [u8; 32] = [1; 32];
    const B: [u8; 32] = [2; 32];

    #[test]
    fn accepts_increasing_and_rejects_duplicates() {
        let mut g = ReplayGuard::default();
        for c in [1, 2, 3, 10, 11] {
            g.check_and_record(&A, c).unwrap();
        }
        for c in [1, 3, 10, 11] {
            assert!(
                matches!(g.check_and_record(&A, c), Err(NoiseError::Replay)),
                "{c}"
            );
        }
    }

    #[test]
    fn accepts_reordering_within_the_window_once() {
        let mut g = ReplayGuard::default();
        g.check_and_record(&A, 100).unwrap();
        g.check_and_record(&A, 98).unwrap();
        g.check_and_record(&A, 99).unwrap();
        assert!(g.check_and_record(&A, 98).is_err());
        assert!(g.check_and_record(&A, 99).is_err());
        assert!(g.check_and_record(&A, 100).is_err());
    }

    #[test]
    fn rejects_counters_older_than_the_window() {
        let mut g = ReplayGuard::default();
        g.check_and_record(&A, 1000).unwrap();
        assert!(
            g.check_and_record(&A, 1000 - 64).is_err(),
            "exactly one window old"
        );
        g.check_and_record(&A, 1000 - 63).unwrap();
        assert!(g.check_and_record(&A, 1).is_err());
    }

    #[test]
    fn large_jumps_reset_the_window() {
        let mut g = ReplayGuard::default();
        g.check_and_record(&A, 5).unwrap();
        g.check_and_record(&A, 5 + 1_000_000).unwrap();
        assert!(
            g.check_and_record(&A, 5).is_err(),
            "now far outside the window"
        );
        g.check_and_record(&A, 5 + 999_999).unwrap();
    }

    #[test]
    fn senders_are_independent() {
        let mut g = ReplayGuard::default();
        g.check_and_record(&A, 7).unwrap();
        g.check_and_record(&B, 7).unwrap();
        assert!(g.check_and_record(&A, 7).is_err());
        assert!(g.check_and_record(&B, 7).is_err());
    }

    #[test]
    fn memory_is_bounded() {
        let mut g = ReplayGuard::new(8);
        for i in 0..100u8 {
            g.check_and_record(&[i; 32], 1).unwrap();
        }
        assert!(g.tracked_senders() <= 8);
    }

    proptest! {
        /// No counter is ever accepted twice, whatever the arrival order.
        #[test]
        fn never_accepts_a_counter_twice(counters in proptest::collection::vec(0u64..200, 0..300)) {
            let mut g = ReplayGuard::default();
            let mut accepted = std::collections::HashSet::new();
            for c in counters {
                if g.check_and_record(&A, c).is_ok() {
                    prop_assert!(accepted.insert(c), "counter {c} accepted twice");
                }
            }
        }
    }
}
