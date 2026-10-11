//! USB HID usage → platform key codes.
//!
//! The wire protocol carries *physical* keys as HID usages (page 0x07). Each platform backend
//! converts to the code its injection API wants:
//!
//! * Linux/X11: evdev code (X keycode = evdev + 8 on every evdev-based server, including Xvfb).
//! * macOS: `kVK_*` virtual keycode for `CGEventCreateKeyboardEvent`.
//! * Windows: set-1 scancode (+ extended flag) for `SendInput(KEYEVENTF_SCANCODE)`, so the
//!   *target's* layout decides what character appears.

use nexus_proto::wire::input::{PAGE_CONSUMER, PAGE_KEYBOARD};

#[path = "hid_table.rs"]
mod table;

/// Codes for one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyInfo {
    /// HID usage (keyboard page).
    pub usage: u16,
    /// Name for logs and tests (US legend).
    pub name: &'static str,
    /// Linux evdev code.
    pub evdev: u16,
    /// macOS virtual keycode; `0xFFFF` if the Mac has no such key.
    pub mac: u16,
    /// Windows set-1 scancode (low byte).
    pub win_scan: u8,
    /// Windows scancode needs the `E0` extended prefix.
    pub win_ext: bool,
}

/// A media key (consumer page).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsumerKey {
    /// HID usage (consumer page).
    pub usage: u16,
    /// Name.
    pub name: &'static str,
    /// Linux evdev code.
    pub evdev: u16,
    /// Windows virtual-key code (`VK_MEDIA_*`, `VK_VOLUME_*`).
    pub win_vk: u8,
    /// macOS `NX_KEYTYPE_*` for system-defined media events.
    pub mac_nx: u8,
}

const CONSUMER: &[ConsumerKey] = &[
    ConsumerKey {
        usage: 0xB5,
        name: "NextTrack",
        evdev: 163,
        win_vk: 0xB0,
        mac_nx: 17,
    },
    ConsumerKey {
        usage: 0xB6,
        name: "PreviousTrack",
        evdev: 165,
        win_vk: 0xB1,
        mac_nx: 18,
    },
    ConsumerKey {
        usage: 0xB7,
        name: "Stop",
        evdev: 166,
        win_vk: 0xB2,
        mac_nx: 0xFF,
    },
    ConsumerKey {
        usage: 0xCD,
        name: "PlayPause",
        evdev: 164,
        win_vk: 0xB3,
        mac_nx: 16,
    },
    ConsumerKey {
        usage: 0xE2,
        name: "Mute",
        evdev: 113,
        win_vk: 0xAD,
        mac_nx: 7,
    },
    ConsumerKey {
        usage: 0xE9,
        name: "VolumeUp",
        evdev: 115,
        win_vk: 0xAF,
        mac_nx: 0,
    },
    ConsumerKey {
        usage: 0xEA,
        name: "VolumeDown",
        evdev: 114,
        win_vk: 0xAE,
        mac_nx: 1,
    },
];

/// Looks up a keyboard-page key.
#[must_use]
pub fn keyboard_key(usage: u16) -> Option<&'static KeyInfo> {
    table::KEYBOARD
        .binary_search_by_key(&usage, |k| k.usage)
        .ok()
        .map(|i| &table::KEYBOARD[i])
}

/// Looks up a consumer-page key.
#[must_use]
pub fn consumer_key(usage: u16) -> Option<&'static ConsumerKey> {
    CONSUMER.iter().find(|k| k.usage == usage)
}

/// The Linux evdev code for any supported (page, usage).
#[must_use]
pub fn evdev_code(page: u8, usage: u16) -> Option<u16> {
    match page {
        PAGE_KEYBOARD => keyboard_key(usage).map(|k| k.evdev),
        PAGE_CONSUMER => consumer_key(usage).map(|k| k.evdev),
        _ => None,
    }
}

/// All keyboard keys, for tests and layout tools.
#[must_use]
pub fn all_keyboard_keys() -> &'static [KeyInfo] {
    table::KEYBOARD
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_unique_and_internally_consistent() {
        let keys = all_keyboard_keys();
        assert!(
            keys.windows(2).all(|w| w[0].usage < w[1].usage),
            "sorted by usage, no duplicates"
        );
        let mut evdev: Vec<u16> = keys.iter().map(|k| k.evdev).collect();
        evdev.sort_unstable();
        evdev.dedup();
        assert_eq!(evdev.len(), keys.len(), "evdev codes are unique");
        // Mac codes are unique except for the "no such key" marker.
        let mut mac: Vec<u16> = keys
            .iter()
            .map(|k| k.mac)
            .filter(|m| *m != 0xFFFF)
            .collect();
        let n = mac.len();
        mac.sort_unstable();
        mac.dedup();
        assert_eq!(mac.len(), n, "mac keycodes are unique");
        // Windows (scan, ext) pairs are unique.
        let mut win: Vec<(u8, bool)> = keys.iter().map(|k| (k.win_scan, k.win_ext)).collect();
        win.sort_unstable();
        win.dedup();
        // Pause and NumLock deliberately share set-1 0x45 (Pause is E1-prefixed in hardware).
        assert!(
            win.len() >= keys.len() - 1,
            "win scancodes collide: {} unique of {}",
            win.len(),
            keys.len()
        );
    }

    #[test]
    fn well_known_keys() {
        let a = keyboard_key(0x04).unwrap();
        assert_eq!(
            (a.name, a.evdev, a.mac, a.win_scan, a.win_ext),
            ("A", 30, 0x00, 0x1E, false)
        );
        let enter = keyboard_key(0x28).unwrap();
        assert_eq!((enter.evdev, enter.mac, enter.win_scan), (28, 0x24, 0x1C));
        let right_ctrl = keyboard_key(0xE4).unwrap();
        assert!(right_ctrl.win_ext);
        assert_eq!(right_ctrl.win_scan, 0x1D);
        let kp_enter = keyboard_key(0x58).unwrap();
        assert_eq!(kp_enter.win_scan, 0x1C);
        assert!(kp_enter.win_ext, "keypad enter is E0 1C, plain enter is 1C");
        let up = keyboard_key(0x52).unwrap();
        assert_eq!(
            (up.evdev, up.mac, up.win_scan, up.win_ext),
            (103, 0x7E, 0x48, true)
        );
        assert!(keyboard_key(0x00).is_none());
        assert!(keyboard_key(0x66).is_none());
    }

    #[test]
    fn all_letters_and_digits_are_present() {
        for usage in 0x04..=0x27 {
            assert!(keyboard_key(usage).is_some(), "usage {usage:#x}");
        }
        for usage in 0x3A..=0x45 {
            assert!(keyboard_key(usage).is_some(), "F-key usage {usage:#x}");
        }
        for usage in 0xE0..=0xE7 {
            assert!(keyboard_key(usage).is_some(), "modifier usage {usage:#x}");
        }
    }

    #[test]
    fn consumer_keys() {
        assert_eq!(evdev_code(PAGE_CONSUMER, 0xCD), Some(164));
        assert_eq!(evdev_code(PAGE_KEYBOARD, 0x04), Some(30));
        assert_eq!(evdev_code(0x99, 0x04), None);
        assert_eq!(consumer_key(0xE9).unwrap().win_vk, 0xAF);
    }
}
