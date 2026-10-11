//! Brakes on a paired-but-misbehaving (or compromised) device.
//!
//! Pairing already means trust, so these are not about strangers: that is the transport
//! allow-list's job. They stop a device that *is* paired from hammering this one with session
//! attempts or from spamming the person at the keyboard with approval prompts.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::devices::Key;

/// Session attempts allowed per peer in [`SESSION_WINDOW`].
pub(crate) const SESSIONS_PER_WINDOW: usize = 12;
pub(crate) const SESSION_WINDOW: Duration = Duration::from_secs(60);
/// Refusals in a row (within [`REFUSAL_WINDOW`]) that trigger a lockout.
pub(crate) const REFUSALS_BEFORE_LOCKOUT: usize = 3;
pub(crate) const REFUSAL_WINDOW: Duration = Duration::from_secs(600);
/// How long a locked-out peer is turned away without bothering anyone.
pub(crate) const LOCKOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Default)]
pub(crate) struct Limits {
    sessions: HashMap<Key, VecDeque<Instant>>,
    refusals: HashMap<Key, VecDeque<Instant>>,
    locked_until: HashMap<Key, Instant>,
}

impl Limits {
    /// Records a session attempt; false if the peer is over its rate.
    pub(crate) fn allow_session(&mut self, peer: Key, now: Instant) -> bool {
        let q = self.sessions.entry(peer).or_default();
        while q
            .front()
            .is_some_and(|t| now.duration_since(*t) > SESSION_WINDOW)
        {
            q.pop_front();
        }
        if q.len() >= SESSIONS_PER_WINDOW {
            return false;
        }
        q.push_back(now);
        true
    }

    /// Is this peer currently locked out of asking for approval?
    pub(crate) fn locked_out(&mut self, peer: &Key, now: Instant) -> bool {
        match self.locked_until.get(peer) {
            Some(until) if *until > now => true,
            Some(_) => {
                self.locked_until.remove(peer);
                false
            }
            None => false,
        }
    }

    /// The person said no (or never answered). Returns true if that tipped the peer into a
    /// lockout.
    pub(crate) fn refused(&mut self, peer: Key, now: Instant) -> bool {
        let q = self.refusals.entry(peer).or_default();
        while q
            .front()
            .is_some_and(|t| now.duration_since(*t) > REFUSAL_WINDOW)
        {
            q.pop_front();
        }
        q.push_back(now);
        if q.len() >= REFUSALS_BEFORE_LOCKOUT {
            q.clear();
            self.locked_until.insert(peer, now + LOCKOUT);
            return true;
        }
        false
    }

    /// The person said yes: the slate is clean.
    pub(crate) fn approved(&mut self, peer: &Key) {
        self.refusals.remove(peer);
        self.locked_until.remove(peer);
    }

    pub(crate) fn forget(&mut self, peer: &Key) {
        self.sessions.remove(peer);
        self.refusals.remove(peer);
        self.locked_until.remove(peer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Key = [1; 32];
    const B: Key = [2; 32];

    #[test]
    fn the_session_rate_is_per_peer_and_recovers() {
        let mut l = Limits::default();
        let t0 = Instant::now();
        for _ in 0..SESSIONS_PER_WINDOW {
            assert!(l.allow_session(A, t0));
        }
        assert!(!l.allow_session(A, t0), "over the rate");
        assert!(l.allow_session(B, t0), "another peer is unaffected");
        assert!(l.allow_session(A, t0 + SESSION_WINDOW + Duration::from_secs(1)));
    }

    #[test]
    fn repeated_refusals_lock_a_peer_out_for_a_while() {
        let mut l = Limits::default();
        let t0 = Instant::now();
        assert!(!l.refused(A, t0));
        assert!(!l.refused(A, t0 + Duration::from_secs(5)));
        assert!(l.refused(A, t0 + Duration::from_secs(10)), "third strike");
        assert!(l.locked_out(&A, t0 + Duration::from_secs(11)));
        assert!(!l.locked_out(&B, t0 + Duration::from_secs(11)));
        assert!(
            !l.locked_out(&A, t0 + LOCKOUT + Duration::from_secs(11)),
            "lockout expires"
        );
    }

    #[test]
    fn old_refusals_are_forgotten_and_a_yes_clears_the_slate() {
        let mut l = Limits::default();
        let t0 = Instant::now();
        l.refused(A, t0);
        l.refused(A, t0);
        // Long after: back to zero, so two more do not lock out.
        let later = t0 + REFUSAL_WINDOW + Duration::from_secs(1);
        assert!(!l.refused(A, later));
        assert!(!l.refused(A, later));
        l.approved(&A);
        assert!(!l.refused(A, later), "the earlier two no longer count");
        l.forget(&A);
        assert!(!l.locked_out(&A, later));
    }
}
