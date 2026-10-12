//! Deciding when to (re)start the agent: a pure state machine, tested on every platform.
//!
//! The Windows service owns exactly one child, the agent running as SYSTEM in the *active
//! console session*. This machine turns events ("the console moved to session 3", "the child
//! died after 4 s", "stop") into actions ("launch into session 3", "kill it", "wait 2 s"), so
//! the rules are testable without a Windows machine.

use std::time::{Duration, Instant};

/// The first retry delay after the child dies; doubles up to [`MAX_BACKOFF`].
pub const MIN_BACKOFF: Duration = Duration::from_secs(1);
/// Longest wait between restarts of a child that keeps dying.
pub const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// A child that ran this long counts as healthy: its next death is treated as a one-off.
pub const HEALTHY_AFTER: Duration = Duration::from_secs(60);
/// How often to look for a console session when there is none (during a user switch).
pub const NO_SESSION_POLL: Duration = Duration::from_secs(1);

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The service is starting. `console` is the active console session, if there is one.
    Start {
        /// The active console session.
        console: Option<u32>,
    },
    /// The console session changed (fast user switch, logon, logoff, RDP connect/disconnect).
    ConsoleChanged {
        /// The new active console session (`None` while Windows switches users).
        console: Option<u32>,
    },
    /// The child exited by itself or because we killed it.
    ChildExited {
        /// How long it had been running.
        ran_for: Duration,
    },
    /// The wait timer expired.
    Timer,
    /// The service is stopping.
    Stop,
}

/// What the service must do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Start the agent in this session.
    Launch(u32),
    /// Terminate the running child.
    Kill,
    /// Do nothing until this long has passed (then deliver [`Event::Timer`]).
    Wait(Duration),
    /// The service can finish.
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// No child, nothing scheduled.
    Idle,
    /// A child runs in this session.
    Running(u32),
    /// We asked the child to die so a new one can start in `then` (or nowhere).
    Replacing {
        then: Option<u32>,
    },
    /// Waiting before restarting a crashed child.
    Backoff,
    /// Stopping; waiting for the child to be gone.
    Stopping,
    Done,
}

/// See the module documentation.
#[derive(Debug)]
pub struct Supervisor {
    state: State,
    console: Option<u32>,
    backoff: Duration,
}

impl Default for Supervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl Supervisor {
    /// A supervisor with nothing running.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: State::Idle,
            console: None,
            backoff: MIN_BACKOFF,
        }
    }

    /// True once [`Action::Exit`] has been issued.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.state == State::Done
    }

    /// The session the child currently runs in.
    #[must_use]
    pub fn running_in(&self) -> Option<u32> {
        match self.state {
            State::Running(s) => Some(s),
            _ => None,
        }
    }

    fn start_or_wait(&mut self) -> Vec<Action> {
        match self.console {
            Some(s) => {
                self.state = State::Running(s);
                vec![Action::Launch(s)]
            }
            None => {
                self.state = State::Idle;
                vec![Action::Wait(NO_SESSION_POLL)]
            }
        }
    }

    /// Feeds one event; returns what to do about it.
    pub fn on_event(&mut self, ev: Event) -> Vec<Action> {
        match (self.state, ev) {
            (State::Done, _) => vec![],

            // ---- stopping wins over everything ----
            (State::Running(_), Event::Stop) => {
                self.state = State::Stopping;
                vec![Action::Kill]
            }
            (State::Replacing { .. }, Event::Stop) => {
                self.state = State::Stopping;
                vec![]
            }
            (State::Stopping, Event::ChildExited { .. })
            | (State::Idle | State::Backoff, Event::Stop) => {
                self.state = State::Done;
                vec![Action::Exit]
            }
            (State::Stopping, _) => vec![],

            // ---- starting ----
            (State::Idle, Event::Start { console }) => {
                self.console = console;
                self.start_or_wait()
            }

            // ---- the console moved ----
            (State::Running(s), Event::ConsoleChanged { console }) => {
                self.console = console;
                if console == Some(s) {
                    vec![]
                } else {
                    // A child in the wrong session sees the wrong desktops: replace it.
                    self.state = State::Replacing { then: console };
                    vec![Action::Kill]
                }
            }
            (State::Replacing { .. }, Event::ConsoleChanged { console }) => {
                self.console = console;
                self.state = State::Replacing { then: console };
                vec![]
            }
            (State::Idle | State::Backoff, Event::ConsoleChanged { console }) => {
                self.console = console;
                if console.is_some() {
                    // Someone logged in: no reason to keep waiting.
                    self.backoff = MIN_BACKOFF;
                    self.start_or_wait()
                } else {
                    vec![Action::Wait(NO_SESSION_POLL)]
                }
            }

            // ---- the child ended ----
            (State::Replacing { then }, Event::ChildExited { .. }) => {
                self.backoff = MIN_BACKOFF;
                self.console = then;
                self.start_or_wait()
            }
            (State::Running(_), Event::ChildExited { ran_for }) => {
                if ran_for >= HEALTHY_AFTER {
                    self.backoff = MIN_BACKOFF;
                }
                let wait = self.backoff;
                self.backoff = (self.backoff * 2).min(MAX_BACKOFF);
                self.state = State::Backoff;
                vec![Action::Wait(wait)]
            }

            // ---- timers ----
            (State::Backoff | State::Idle, Event::Timer) => self.start_or_wait(),

            // Anything else (a stale timer, a duplicate start) changes nothing.
            _ => vec![],
        }
    }
}

/// Convenience for the Windows loop: when does the next timer fire?
#[must_use]
pub fn deadline(now: Instant, actions: &[Action]) -> Option<Instant> {
    actions.iter().find_map(|a| match a {
        Action::Wait(d) => Some(now + *d),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(console: Option<u32>) -> (Supervisor, Vec<Action>) {
        let mut s = Supervisor::new();
        let a = s.on_event(Event::Start { console });
        (s, a)
    }

    #[test]
    fn starts_in_the_console_session() {
        let (s, a) = started(Some(2));
        assert_eq!(a, [Action::Launch(2)]);
        assert_eq!(s.running_in(), Some(2));
    }

    #[test]
    fn with_no_console_session_it_polls_until_one_appears() {
        let (mut s, a) = started(None);
        assert_eq!(a, [Action::Wait(NO_SESSION_POLL)]);
        assert_eq!(s.on_event(Event::Timer), [Action::Wait(NO_SESSION_POLL)]);
        assert_eq!(
            s.on_event(Event::ConsoleChanged { console: Some(1) }),
            [Action::Launch(1)]
        );
    }

    #[test]
    fn a_fast_user_switch_replaces_the_child() {
        let (mut s, _) = started(Some(1));
        assert_eq!(
            s.on_event(Event::ConsoleChanged { console: Some(3) }),
            [Action::Kill]
        );
        assert_eq!(s.running_in(), None);
        assert_eq!(
            s.on_event(Event::ChildExited {
                ran_for: Duration::from_secs(5)
            }),
            [Action::Launch(3)]
        );
        assert_eq!(s.running_in(), Some(3));
    }

    #[test]
    fn lock_and_unlock_in_the_same_session_do_nothing() {
        let (mut s, _) = started(Some(1));
        assert!(
            s.on_event(Event::ConsoleChanged { console: Some(1) })
                .is_empty()
        );
        assert_eq!(s.running_in(), Some(1));
    }

    #[test]
    fn the_console_vanishing_stops_the_child_and_waits() {
        let (mut s, _) = started(Some(1));
        assert_eq!(
            s.on_event(Event::ConsoleChanged { console: None }),
            [Action::Kill]
        );
        assert_eq!(
            s.on_event(Event::ChildExited {
                ran_for: Duration::from_secs(9)
            }),
            [Action::Wait(NO_SESSION_POLL)]
        );
        assert_eq!(
            s.on_event(Event::ConsoleChanged { console: Some(2) }),
            [Action::Launch(2)]
        );
    }

    #[test]
    fn a_crashing_child_is_restarted_with_growing_delays_capped() {
        let (mut s, _) = started(Some(1));
        let quick = Duration::from_secs(1);
        let mut waits = Vec::new();
        for _ in 0..8 {
            let a = s.on_event(Event::ChildExited { ran_for: quick });
            let Action::Wait(d) = a[0] else {
                panic!("{a:?}")
            };
            waits.push(d);
            assert_eq!(s.on_event(Event::Timer), [Action::Launch(1)]);
        }
        assert_eq!(waits[0], MIN_BACKOFF);
        assert_eq!(waits[1], MIN_BACKOFF * 2);
        assert_eq!(waits[2], MIN_BACKOFF * 4);
        assert_eq!(*waits.last().unwrap(), MAX_BACKOFF);
        assert!(waits.windows(2).all(|w| w[1] >= w[0]));
    }

    #[test]
    fn a_healthy_child_resets_the_delay() {
        let (mut s, _) = started(Some(1));
        for _ in 0..4 {
            s.on_event(Event::ChildExited {
                ran_for: Duration::from_secs(1),
            });
            s.on_event(Event::Timer);
        }
        let a = s.on_event(Event::ChildExited {
            ran_for: HEALTHY_AFTER,
        });
        assert_eq!(a, [Action::Wait(MIN_BACKOFF)]);
    }

    #[test]
    fn a_logon_during_backoff_restarts_at_once() {
        let (mut s, _) = started(Some(1));
        s.on_event(Event::ChildExited {
            ran_for: Duration::from_secs(1),
        });
        assert_eq!(
            s.on_event(Event::ConsoleChanged { console: Some(4) }),
            [Action::Launch(4)]
        );
    }

    #[test]
    fn stop_kills_waits_for_the_exit_then_finishes() {
        let (mut s, _) = started(Some(1));
        assert_eq!(s.on_event(Event::Stop), [Action::Kill]);
        assert!(!s.is_done());
        // Console changes while stopping are ignored.
        assert!(
            s.on_event(Event::ConsoleChanged { console: Some(2) })
                .is_empty()
        );
        assert_eq!(
            s.on_event(Event::ChildExited {
                ran_for: Duration::from_secs(3)
            }),
            [Action::Exit]
        );
        assert!(s.is_done());
        assert!(s.on_event(Event::Timer).is_empty());
    }

    #[test]
    fn stop_when_idle_or_backing_off_exits_immediately() {
        let (mut s, _) = started(None);
        assert_eq!(s.on_event(Event::Stop), [Action::Exit]);
        let (mut s, _) = started(Some(1));
        s.on_event(Event::ChildExited {
            ran_for: Duration::from_secs(1),
        });
        assert_eq!(s.on_event(Event::Stop), [Action::Exit]);
    }

    #[test]
    fn stop_while_replacing_still_ends_cleanly() {
        let (mut s, _) = started(Some(1));
        s.on_event(Event::ConsoleChanged { console: Some(2) });
        assert!(s.on_event(Event::Stop).is_empty());
        assert_eq!(
            s.on_event(Event::ChildExited {
                ran_for: Duration::from_secs(1)
            }),
            [Action::Exit]
        );
    }

    #[test]
    fn a_session_change_while_replacing_retargets() {
        let (mut s, _) = started(Some(1));
        s.on_event(Event::ConsoleChanged { console: Some(2) });
        assert!(
            s.on_event(Event::ConsoleChanged { console: Some(5) })
                .is_empty()
        );
        assert_eq!(
            s.on_event(Event::ChildExited {
                ran_for: Duration::from_secs(1)
            }),
            [Action::Launch(5)]
        );
    }

    #[test]
    fn deadline_helper() {
        let now = Instant::now();
        assert_eq!(deadline(now, &[Action::Kill]), None);
        assert_eq!(
            deadline(now, &[Action::Wait(Duration::from_secs(2))]),
            Some(now + Duration::from_secs(2))
        );
    }
}
