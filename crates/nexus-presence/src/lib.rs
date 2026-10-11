//! What this machine is doing, in the terms the dashboard shows: *in use*, *idle*, *locked*,
//! *online* (awake with nobody around), plus battery.
//!
//! [`classify`] is a pure function of a [`Snapshot`], so the state machine is tested everywhere;
//! the per-OS code only produces snapshots. Where an OS cannot say (a headless server has no
//! idle time), the snapshot says so and the device is simply *online*.

#![deny(unsafe_code)]

use std::time::Duration;

use nexus_proto::v1::PresenceState;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod windows_impl;

pub mod parse;

/// A reading of the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Time since the last keyboard/mouse input at the machine. `None` = unknown.
    pub idle: Option<Duration>,
    /// Is the screen locked?
    pub locked: bool,
    /// The interactive user, if there is one.
    pub user: Option<String>,
    /// Battery charge, if there is a battery.
    pub battery_percent: Option<u8>,
    /// Running on mains power?
    pub on_ac: bool,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            idle: None,
            locked: false,
            user: None,
            battery_percent: None,
            on_ac: true,
        }
    }
}

/// Thresholds for [`classify`].
#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    /// Input within this long counts as someone using the machine now.
    pub in_use_within: Duration,
    /// No input for this long (but not locked) is *idle*.
    pub idle_after: Duration,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            in_use_within: Duration::from_secs(60),
            idle_after: Duration::from_secs(5 * 60),
        }
    }
}

/// Turns a reading into a presence state.
///
/// * a remote session in progress is *in use*, whatever the local keyboard did;
/// * a locked screen is *locked*;
/// * recent local input is *in use*, a long silence is *idle*;
/// * a machine with no interactive user, or no way to tell, is just *online*.
#[must_use]
pub fn classify(s: &Snapshot, remote_session: bool, t: Thresholds) -> PresenceState {
    if remote_session {
        return PresenceState::InUse;
    }
    if s.locked {
        return PresenceState::Locked;
    }
    match (&s.user, s.idle) {
        (Some(_), Some(idle)) if idle <= t.in_use_within => PresenceState::InUse,
        (Some(_), Some(idle)) if idle >= t.idle_after => PresenceState::Idle,
        // Between the two thresholds the user is around but not typing: still "in use".
        (Some(_), Some(_)) => PresenceState::InUse,
        _ => PresenceState::Online,
    }
}

/// Reads the machine.
pub trait Sensor: Send + Sync + std::fmt::Debug {
    /// Takes a reading (cheap; called every few seconds).
    fn sample(&self) -> Snapshot;
}

/// A sensor that returns a fixed reading (tests, platforms without support).
#[derive(Debug, Clone, Default)]
pub struct FixedSensor(pub Snapshot);

impl Sensor for FixedSensor {
    fn sample(&self) -> Snapshot {
        self.0.clone()
    }
}

/// The sensor for the OS this build runs on.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemSensor;

impl Sensor for SystemSensor {
    fn sample(&self) -> Snapshot {
        #[cfg(target_os = "linux")]
        return linux::sample();
        #[cfg(target_os = "macos")]
        return macos::sample();
        #[cfg(target_os = "windows")]
        return windows_impl::sample();
        #[allow(unreachable_code)]
        Snapshot::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(user: bool, idle: Option<u64>, locked: bool) -> Snapshot {
        Snapshot {
            idle: idle.map(Duration::from_secs),
            locked,
            user: user.then(|| "me".to_owned()),
            ..Snapshot::default()
        }
    }

    #[test]
    fn the_state_machine_covers_the_cases() {
        let t = Thresholds::default();
        let st = |s: &Snapshot, remote| classify(s, remote, t);
        assert_eq!(st(&snap(true, Some(3), false), false), PresenceState::InUse);
        assert_eq!(
            st(&snap(true, Some(120), false), false),
            PresenceState::InUse
        );
        assert_eq!(
            st(&snap(true, Some(301), false), false),
            PresenceState::Idle
        );
        assert_eq!(st(&snap(true, Some(1), true), false), PresenceState::Locked);
        assert_eq!(
            st(&snap(true, Some(900), true), false),
            PresenceState::Locked
        );
        // A remote session beats everything.
        assert_eq!(st(&snap(true, Some(900), true), true), PresenceState::InUse);
        // No user or no idle info: simply online.
        assert_eq!(
            st(&snap(false, Some(900), false), false),
            PresenceState::Online
        );
        assert_eq!(st(&snap(true, None, false), false), PresenceState::Online);
        assert_eq!(st(&Snapshot::default(), false), PresenceState::Online);
    }

    #[test]
    fn this_machine_can_be_read_without_panicking() {
        let s = SystemSensor.sample();
        // Whatever it says, it is internally consistent.
        if let Some(b) = s.battery_percent {
            assert!(b <= 100);
        }
        assert_eq!(FixedSensor(s.clone()).sample(), s);
    }
}
