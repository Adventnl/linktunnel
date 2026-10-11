//! An injector that records instead of injecting, for tests.

use std::sync::{Arc, Mutex};

use nexus_proto::wire::input::InputEvent;

use crate::{Injector, InputError, MonitorRect};

/// Shared log of injected events.
#[derive(Debug, Clone, Default)]
pub struct Recorded(Arc<Mutex<Vec<InputEvent>>>);

impl Recorded {
    /// Everything injected so far.
    #[must_use]
    pub fn events(&self) -> Vec<InputEvent> {
        self.0.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Clears the log.
    pub fn clear(&self) {
        if let Ok(mut g) = self.0.lock() {
            g.clear();
        }
    }
}

/// Records events; see [`Recorded`].
#[derive(Debug)]
pub struct RecordingInjector {
    log: Recorded,
    monitors: Vec<MonitorRect>,
}

impl RecordingInjector {
    /// Creates an injector and the handle to read what it recorded.
    #[must_use]
    pub fn new() -> (Self, Recorded) {
        let log = Recorded::default();
        (
            Self {
                log: log.clone(),
                monitors: Vec::new(),
            },
            log,
        )
    }

    /// Monitors last announced via `set_monitors`.
    #[must_use]
    pub fn monitors(&self) -> &[MonitorRect] {
        &self.monitors
    }
}

impl Injector for RecordingInjector {
    fn name(&self) -> &'static str {
        "recording"
    }

    fn set_monitors(&mut self, monitors: &[MonitorRect]) {
        self.monitors = monitors.to_vec();
    }

    fn inject(&mut self, event: &InputEvent) -> Result<(), InputError> {
        self.log
            .0
            .lock()
            .map_err(|_| InputError::Failed("log poisoned".into()))?
            .push(event.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use nexus_proto::wire::input::{KeyState, Modifiers, MouseButton};

    use super::*;
    use crate::Guarded;

    fn key(usage: u16, state: KeyState) -> InputEvent {
        InputEvent::Key {
            page: 7,
            usage,
            state,
            modifiers: Modifiers::default(),
        }
    }

    #[test]
    fn guard_releases_everything_held_in_a_defined_order() {
        let (inj, log) = RecordingInjector::new();
        let mut g = Guarded::new(Box::new(inj));
        g.inject(&key(0xE1, KeyState::Down)).unwrap(); // shift
        g.inject(&key(0x04, KeyState::Down)).unwrap(); // a
        g.inject(&InputEvent::MouseButton {
            button: MouseButton::LEFT,
            pressed: true,
        })
        .unwrap();
        g.inject(&key(0x04, KeyState::Up)).unwrap();
        assert_eq!(g.held(), 2);
        log.clear();

        g.inject(&InputEvent::ReleaseAll).unwrap();
        assert_eq!(g.held(), 0);
        assert_eq!(
            log.events(),
            vec![
                key(0xE1, KeyState::Up),
                InputEvent::MouseButton {
                    button: MouseButton::LEFT,
                    pressed: false
                }
            ]
        );
    }

    #[test]
    fn dropping_the_guard_releases_held_input() {
        let (inj, log) = RecordingInjector::new();
        {
            let mut g = Guarded::new(Box::new(inj));
            g.inject(&key(0xE0, KeyState::Down)).unwrap();
        }
        assert_eq!(log.events().last(), Some(&key(0xE0, KeyState::Up)));
    }

    #[test]
    fn repeat_counts_as_held_and_up_clears_it() {
        let (inj, _log) = RecordingInjector::new();
        let mut g = Guarded::new(Box::new(inj));
        g.inject(&key(0x2C, KeyState::Repeat)).unwrap();
        assert_eq!(g.held(), 1);
        g.inject(&key(0x2C, KeyState::Up)).unwrap();
        assert_eq!(g.held(), 0);
    }
}
