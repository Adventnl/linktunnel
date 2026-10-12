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

/// Most peers whose resume state is remembered (a paired set is tiny; this only bounds abuse).
const MAX_RESUME_ENTRIES: usize = 256;

#[derive(Debug, Default)]
pub(crate) struct Limits {
    sessions: HashMap<Key, VecDeque<Instant>>,
    refusals: HashMap<Key, VecDeque<Instant>>,
    locked_until: HashMap<Key, Instant>,
    /// When a person last said yes to this peer, kept fresh while its session lives. A session
    /// that drops (Wi-Fi switch, sleep, brief outage) may come back within the resume window
    /// without asking again; a session that *ended on purpose* clears this.
    last_ok: HashMap<Key, Instant>,
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

    /// The person said yes: the slate is clean, and a dropped link may resume for a while.
    pub(crate) fn approved(&mut self, peer: &Key, now: Instant) {
        self.refusals.remove(peer);
        self.locked_until.remove(peer);
        self.note_alive(*peer, now);
    }

    /// The approved peer's session is still alive: keep its resume window open. Does nothing
    /// for a peer that has no approval on record (an unattended device never needs one).
    pub(crate) fn heartbeat(&mut self, peer: &Key, now: Instant) {
        if let Some(t) = self.last_ok.get_mut(peer) {
            *t = now;
        }
    }

    fn note_alive(&mut self, peer: Key, now: Instant) {
        if self.last_ok.len() >= MAX_RESUME_ENTRIES && !self.last_ok.contains_key(&peer) {
            self.last_ok
                .retain(|_, t| now.duration_since(*t) < Duration::from_secs(3600));
        }
        if self.last_ok.len() < MAX_RESUME_ENTRIES {
            self.last_ok.insert(peer, now);
        }
    }

    /// May a new session from this peer skip the prompt because its last approved session only
    /// lost its link a moment ago?
    pub(crate) fn may_resume(&self, peer: &Key, now: Instant, window: Duration) -> bool {
        !window.is_zero()
            && self
                .last_ok
                .get(peer)
                .is_some_and(|t| now.duration_since(*t) < window)
    }

    /// The session ended on purpose (goodbye, refusal, shutdown): the next one must ask again.
    pub(crate) fn ended_on_purpose(&mut self, peer: &Key) {
        self.last_ok.remove(peer);
    }

    pub(crate) fn forget(&mut self, peer: &Key) {
        self.last_ok.remove(peer);
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
        l.approved(&A, later);
        assert!(!l.refused(A, later), "the earlier two no longer count");
        l.forget(&A);
        assert!(!l.locked_out(&A, later));
    }

    #[test]
    fn a_dropped_link_may_resume_briefly_and_a_deliberate_end_may_not() {
        let mut l = Limits::default();
        let (t0, w) = (Instant::now(), Duration::from_secs(120));
        assert!(!l.may_resume(&A, t0, w), "never approved");
        l.approved(&A, t0);
        assert!(l.may_resume(&A, t0 + Duration::from_secs(100), w));
        assert!(
            !l.may_resume(&A, t0 + Duration::from_secs(121), w),
            "window over"
        );
        // While the session lives, the window keeps sliding; it starts counting at the drop.
        l.heartbeat(&A, t0 + Duration::from_secs(300));
        assert!(l.may_resume(&A, t0 + Duration::from_secs(400), w));
        assert!(!l.may_resume(&A, t0 + Duration::from_secs(421), w));
        // A goodbye ends it for good.
        l.heartbeat(&A, t0 + Duration::from_secs(500));
        l.ended_on_purpose(&A);
        assert!(!l.may_resume(&A, t0 + Duration::from_secs(501), w));
        // Other peers are independent, a zero window disables the feature, and a heartbeat for an
        // unapproved peer does not invent an approval.
        l.heartbeat(&B, t0);
        assert!(!l.may_resume(&B, t0, w));
        l.approved(&B, t0);
        assert!(!l.may_resume(&B, t0, Duration::ZERO));
    }

    #[test]
    fn resume_state_is_bounded() {
        let mut l = Limits::default();
        let t0 = Instant::now();
        for i in 0..(MAX_RESUME_ENTRIES + 50) {
            let mut k = [0u8; 32];
            k[..8].copy_from_slice(&(i as u64).to_le_bytes());
            l.approved(&k, t0);
        }
        assert!(l.last_ok.len() <= MAX_RESUME_ENTRIES);
    }
}
