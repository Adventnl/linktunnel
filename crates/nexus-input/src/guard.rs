use std::collections::BTreeSet;

use nexus_proto::wire::input::{InputEvent, KeyState};

use crate::{Injector, InputError, MonitorRect};

/// Wraps an [`Injector`], tracking held keys and buttons so they can all be released.
pub struct Guarded {
    inner: Box<dyn Injector>,
    keys: BTreeSet<(u8, u16)>,
    buttons: BTreeSet<u8>,
}

impl std::fmt::Debug for Guarded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Guarded")
            .field("backend", &self.inner.name())
            .field("held_keys", &self.keys.len())
            .field("held_buttons", &self.buttons.len())
            .finish()
    }
}

impl Guarded {
    /// Wraps `inner`.
    #[must_use]
    pub fn new(inner: Box<dyn Injector>) -> Self {
        Self {
            inner,
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
        }
    }

    /// Backend name.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.inner.name()
    }

    /// See [`Injector::set_monitors`].
    pub fn set_monitors(&mut self, monitors: &[MonitorRect]) {
        self.inner.set_monitors(monitors);
    }

    /// Number of keys and buttons currently held down.
    #[must_use]
    pub fn held(&self) -> usize {
        self.keys.len() + self.buttons.len()
    }

    /// Performs `event`, updating the held-state bookkeeping. `ReleaseAll` is handled here.
    pub fn inject(&mut self, event: &InputEvent) -> Result<(), InputError> {
        match event {
            InputEvent::ReleaseAll => return self.release_all(),
            InputEvent::Key {
                page, usage, state, ..
            } => {
                let id = (*page, *usage);
                match state {
                    KeyState::Down | KeyState::Repeat => {
                        self.keys.insert(id);
                    }
                    KeyState::Up => {
                        self.keys.remove(&id);
                    }
                }
            }
            InputEvent::MouseButton { button, pressed } => {
                if *pressed {
                    self.buttons.insert(button.0);
                } else {
                    self.buttons.remove(&button.0);
                }
            }
            _ => {}
        }
        self.inner.inject(event)
    }

    /// Releases everything held. Always attempts every release, returning the first error.
    pub fn release_all(&mut self) -> Result<(), InputError> {
        let mut first = None;
        for (page, usage) in std::mem::take(&mut self.keys) {
            let up = InputEvent::Key {
                page,
                usage,
                state: KeyState::Up,
                modifiers: nexus_proto::wire::input::Modifiers::default(),
            };
            if let Err(e) = self.inner.inject(&up) {
                first.get_or_insert(e);
            }
        }
        for button in std::mem::take(&mut self.buttons) {
            let up = InputEvent::MouseButton {
                button: nexus_proto::wire::input::MouseButton(button),
                pressed: false,
            };
            if let Err(e) = self.inner.inject(&up) {
                first.get_or_insert(e);
            }
        }
        first.map_or(Ok(()), Err)
    }
}

impl Drop for Guarded {
    fn drop(&mut self) {
        let _ = self.release_all();
    }
}
