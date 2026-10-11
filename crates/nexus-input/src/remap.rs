//! Modifier remapping on the controller, so a Mac's ⌘C reaches a Windows PC as Ctrl+C and the
//! other way round. Applied to keyboard-page HID usages before they go on the wire.

use std::collections::BTreeMap;

use nexus_proto::wire::input::{InputEvent, PAGE_KEYBOARD};

const CTRL_L: u16 = 0xE0;
const ALT_L: u16 = 0xE2;
const META_L: u16 = 0xE3;
const CTRL_R: u16 = 0xE4;
const ALT_R: u16 = 0xE6;
const META_R: u16 = 0xE7;
const CAPS: u16 = 0x39;

/// A usage → usage substitution table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyRemap {
    map: BTreeMap<u16, u16>,
}

impl KeyRemap {
    /// No remapping.
    #[must_use]
    pub fn identity() -> Self {
        Self::default()
    }

    /// Mac controller → Windows/Linux target: ⌘ acts as Ctrl, Ctrl acts as the Windows/Super key,
    /// ⌥ stays Alt. The result is that ⌘C / ⌘V / ⌘T behave as the muscle memory expects.
    #[must_use]
    pub fn mac_to_pc() -> Self {
        Self::from_pairs(&[
            (META_L, CTRL_L),
            (META_R, CTRL_R),
            (CTRL_L, META_L),
            (CTRL_R, META_R),
        ])
    }

    /// The reverse, for a PC keyboard controlling a Mac: Ctrl acts as ⌘, the Windows key as Ctrl.
    #[must_use]
    pub fn pc_to_mac() -> Self {
        Self::from_pairs(&[
            (CTRL_L, META_L),
            (CTRL_R, META_R),
            (META_L, CTRL_L),
            (META_R, CTRL_R),
        ])
    }

    /// Builds a table from explicit pairs.
    #[must_use]
    pub fn from_pairs(pairs: &[(u16, u16)]) -> Self {
        Self {
            map: pairs.iter().copied().collect(),
        }
    }

    /// Also makes Caps Lock act as Ctrl.
    #[must_use]
    pub fn with_caps_as_ctrl(mut self) -> Self {
        self.map.insert(CAPS, CTRL_L);
        self
    }

    /// Alt ↔ Meta swap (for people who want ⌥ to be the Windows key and vice versa).
    #[must_use]
    pub fn with_alt_meta_swapped(mut self) -> Self {
        for (a, b) in [(ALT_L, META_L), (ALT_R, META_R)] {
            self.map.insert(a, b);
            self.map.insert(b, a);
        }
        self
    }

    /// The usage to send for `usage`.
    #[must_use]
    pub fn apply_usage(&self, usage: u16) -> u16 {
        self.map.get(&usage).copied().unwrap_or(usage)
    }

    /// Rewrites keyboard events in place.
    pub fn apply(&self, event: &mut InputEvent) {
        if let InputEvent::Key {
            page: PAGE_KEYBOARD,
            usage,
            ..
        } = event
        {
            *usage = self.apply_usage(*usage);
        }
    }

    /// True when nothing is remapped.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use nexus_proto::wire::input::{KeyState, Modifiers};

    use super::*;

    fn key(page: u8, usage: u16) -> InputEvent {
        InputEvent::Key {
            page,
            usage,
            state: KeyState::Down,
            modifiers: Modifiers::default(),
        }
    }

    #[test]
    fn mac_to_pc_swaps_command_and_control() {
        let r = KeyRemap::mac_to_pc();
        assert_eq!(r.apply_usage(META_L), CTRL_L);
        assert_eq!(r.apply_usage(CTRL_L), META_L);
        assert_eq!(r.apply_usage(0x06), 0x06, "letters are untouched");
        assert!(KeyRemap::identity().is_identity());
    }

    #[test]
    fn only_keyboard_page_keys_are_rewritten() {
        let r = KeyRemap::mac_to_pc();
        let mut consumer = key(0x0C, META_L);
        r.apply(&mut consumer);
        assert_eq!(consumer, key(0x0C, META_L));
        let mut kb = key(PAGE_KEYBOARD, META_L);
        r.apply(&mut kb);
        assert_eq!(kb, key(PAGE_KEYBOARD, CTRL_L));
    }

    #[test]
    fn presets_compose() {
        let r = KeyRemap::identity()
            .with_caps_as_ctrl()
            .with_alt_meta_swapped();
        assert_eq!(r.apply_usage(CAPS), CTRL_L);
        assert_eq!(r.apply_usage(ALT_L), META_L);
        assert_eq!(r.apply_usage(META_L), ALT_L);
    }
}
