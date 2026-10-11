//! Modifier remapping on the controller, so a Mac's ⌘C reaches a Windows PC as Ctrl+C and the
//! other way round. Applied to keyboard-page HID usages before they go on the wire.

use std::collections::BTreeMap;

use nexus_proto::v1::OsFamily;
use nexus_proto::wire::input::{InputEvent, Modifiers, PAGE_KEYBOARD};

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

    /// The sensible default for a controller running `controller` driving a `target`: a Mac
    /// keyboard on a PC (or the reverse) swaps ⌘ and Ctrl so shortcuts keep their muscle memory;
    /// every other pairing is left alone.
    #[must_use]
    pub fn auto(controller: OsFamily, target: OsFamily) -> Self {
        use OsFamily::{Linux, Macos, Windows};
        match (controller, target) {
            (Macos, Windows | Linux) => Self::mac_to_pc(),
            (Windows | Linux, Macos) => Self::pc_to_mac(),
            _ => Self::identity(),
        }
    }

    /// The usage to send for `usage`.
    #[must_use]
    pub fn apply_usage(&self, usage: u16) -> u16 {
        self.map.get(&usage).copied().unwrap_or(usage)
    }

    /// Rewrites keyboard events in place: the key itself and the modifier state it carries.
    pub fn apply(&self, event: &mut InputEvent) {
        if let InputEvent::Key {
            page: PAGE_KEYBOARD,
            usage,
            modifiers,
            ..
        } = event
        {
            *usage = self.apply_usage(*usage);
            *modifiers = self.apply_modifiers(*modifiers);
        }
    }

    /// Maps a modifier set through the table (Ctrl→Meta etc.).
    #[must_use]
    pub fn apply_modifiers(&self, m: Modifiers) -> Modifiers {
        // Each logical modifier goes where its left key goes.
        let target_bit = |usage: u16| match self.apply_usage(usage) {
            CTRL_L | CTRL_R => Modifiers::CTRL,
            ALT_L | ALT_R => Modifiers::ALT,
            META_L | META_R => Modifiers::META,
            _ => Modifiers(0),
        };
        let mut out = Modifiers(m.0 & !(Modifiers::CTRL.0 | Modifiers::ALT.0 | Modifiers::META.0));
        for (bit, usage) in [
            (Modifiers::CTRL, CTRL_L),
            (Modifiers::ALT, ALT_L),
            (Modifiers::META, META_L),
        ] {
            if m.contains(bit) {
                out = out.with(target_bit(usage));
            }
        }
        out
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
    fn modifier_state_follows_the_keys() {
        let r = KeyRemap::mac_to_pc();
        let mut e = InputEvent::Key {
            page: PAGE_KEYBOARD,
            usage: 0x06, // C
            state: KeyState::Down,
            modifiers: Modifiers::META.with(Modifiers::SHIFT),
        };
        r.apply(&mut e);
        assert_eq!(
            e,
            InputEvent::Key {
                page: PAGE_KEYBOARD,
                usage: 0x06,
                state: KeyState::Down,
                modifiers: Modifiers::CTRL.with(Modifiers::SHIFT),
            }
        );
        assert_eq!(
            r.apply_modifiers(Modifiers::CTRL.with(Modifiers::ALT)),
            Modifiers::META.with(Modifiers::ALT)
        );
        assert_eq!(
            KeyRemap::identity().apply_modifiers(Modifiers::CTRL),
            Modifiers::CTRL
        );
    }

    #[test]
    fn the_default_depends_on_the_os_pair() {
        use OsFamily::{Linux, Macos, Windows};
        assert_eq!(KeyRemap::auto(Macos, Windows), KeyRemap::mac_to_pc());
        assert_eq!(KeyRemap::auto(Macos, Linux), KeyRemap::mac_to_pc());
        assert_eq!(KeyRemap::auto(Windows, Macos), KeyRemap::pc_to_mac());
        assert!(KeyRemap::auto(Windows, Linux).is_identity());
        assert!(KeyRemap::auto(Macos, Macos).is_identity());
        assert!(KeyRemap::auto(Linux, Windows).is_identity());
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
