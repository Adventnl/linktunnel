//! winit physical keys → HID usages (the wire format for keys).

use nexus_proto::wire::input::{PAGE_CONSUMER, PAGE_KEYBOARD};
use winit::keyboard::KeyCode;

/// The HID `(page, usage)` for a physical key, if the protocol can express it.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn hid_for(code: KeyCode) -> Option<(u8, u16)> {
    use KeyCode::*;
    let kb = |u: u16| Some((PAGE_KEYBOARD, u));
    let media = |u: u16| Some((PAGE_CONSUMER, u));
    match code {
        KeyA => kb(0x04),
        KeyB => kb(0x05),
        KeyC => kb(0x06),
        KeyD => kb(0x07),
        KeyE => kb(0x08),
        KeyF => kb(0x09),
        KeyG => kb(0x0A),
        KeyH => kb(0x0B),
        KeyI => kb(0x0C),
        KeyJ => kb(0x0D),
        KeyK => kb(0x0E),
        KeyL => kb(0x0F),
        KeyM => kb(0x10),
        KeyN => kb(0x11),
        KeyO => kb(0x12),
        KeyP => kb(0x13),
        KeyQ => kb(0x14),
        KeyR => kb(0x15),
        KeyS => kb(0x16),
        KeyT => kb(0x17),
        KeyU => kb(0x18),
        KeyV => kb(0x19),
        KeyW => kb(0x1A),
        KeyX => kb(0x1B),
        KeyY => kb(0x1C),
        KeyZ => kb(0x1D),
        Digit1 => kb(0x1E),
        Digit2 => kb(0x1F),
        Digit3 => kb(0x20),
        Digit4 => kb(0x21),
        Digit5 => kb(0x22),
        Digit6 => kb(0x23),
        Digit7 => kb(0x24),
        Digit8 => kb(0x25),
        Digit9 => kb(0x26),
        Digit0 => kb(0x27),
        Enter => kb(0x28),
        Escape => kb(0x29),
        Backspace => kb(0x2A),
        Tab => kb(0x2B),
        Space => kb(0x2C),
        Minus => kb(0x2D),
        Equal => kb(0x2E),
        BracketLeft => kb(0x2F),
        BracketRight => kb(0x30),
        Backslash => kb(0x31),
        Semicolon => kb(0x33),
        Quote => kb(0x34),
        Backquote => kb(0x35),
        Comma => kb(0x36),
        Period => kb(0x37),
        Slash => kb(0x38),
        CapsLock => kb(0x39),
        F1 => kb(0x3A),
        F2 => kb(0x3B),
        F3 => kb(0x3C),
        F4 => kb(0x3D),
        F5 => kb(0x3E),
        F6 => kb(0x3F),
        F7 => kb(0x40),
        F8 => kb(0x41),
        F9 => kb(0x42),
        F10 => kb(0x43),
        F11 => kb(0x44),
        F12 => kb(0x45),
        PrintScreen => kb(0x46),
        ScrollLock => kb(0x47),
        Pause => kb(0x48),
        Insert => kb(0x49),
        Home => kb(0x4A),
        PageUp => kb(0x4B),
        Delete => kb(0x4C),
        End => kb(0x4D),
        PageDown => kb(0x4E),
        ArrowRight => kb(0x4F),
        ArrowLeft => kb(0x50),
        ArrowDown => kb(0x51),
        ArrowUp => kb(0x52),
        NumLock => kb(0x53),
        NumpadDivide => kb(0x54),
        NumpadMultiply => kb(0x55),
        NumpadSubtract => kb(0x56),
        NumpadAdd => kb(0x57),
        NumpadEnter => kb(0x58),
        Numpad1 => kb(0x59),
        Numpad2 => kb(0x5A),
        Numpad3 => kb(0x5B),
        Numpad4 => kb(0x5C),
        Numpad5 => kb(0x5D),
        Numpad6 => kb(0x5E),
        Numpad7 => kb(0x5F),
        Numpad8 => kb(0x60),
        Numpad9 => kb(0x61),
        Numpad0 => kb(0x62),
        NumpadDecimal => kb(0x63),
        IntlBackslash => kb(0x64),
        ContextMenu => kb(0x65),
        F13 => kb(0x68),
        F14 => kb(0x69),
        F15 => kb(0x6A),
        F16 => kb(0x6B),
        F17 => kb(0x6C),
        F18 => kb(0x6D),
        F19 => kb(0x6E),
        F20 => kb(0x6F),
        F21 => kb(0x70),
        F22 => kb(0x71),
        F23 => kb(0x72),
        F24 => kb(0x73),
        ControlLeft => kb(0xE0),
        ShiftLeft => kb(0xE1),
        AltLeft => kb(0xE2),
        SuperLeft => kb(0xE3),
        ControlRight => kb(0xE4),
        ShiftRight => kb(0xE5),
        AltRight => kb(0xE6),
        SuperRight => kb(0xE7),
        MediaTrackNext => media(0xB5),
        MediaTrackPrevious => media(0xB6),
        MediaStop => media(0xB7),
        MediaPlayPause => media(0xCD),
        AudioVolumeMute => media(0xE2),
        AudioVolumeUp => media(0xE9),
        AudioVolumeDown => media(0xEA),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use nexus_input::hid;

    use super::*;

    #[test]
    fn every_mapped_key_exists_in_the_injection_tables() {
        // If the viewer can send a usage, the target must be able to inject it.
        use KeyCode::*;
        for code in [
            KeyA,
            KeyZ,
            Digit1,
            Digit0,
            Enter,
            F1,
            F12,
            F24,
            ArrowUp,
            ControlLeft,
            MetaOrSuper(),
            NumpadEnter,
            ContextMenu,
            IntlBackslash,
            PrintScreen,
        ] {
            let Some((page, usage)) = hid_for(code) else {
                continue;
            };
            assert!(
                hid::evdev_code(page, usage).is_some(),
                "{code:?} -> {page:#x}:{usage:#x} has no injection mapping"
            );
        }
        for code in [
            MediaPlayPause,
            AudioVolumeUp,
            AudioVolumeDown,
            AudioVolumeMute,
            MediaTrackNext,
        ] {
            let (page, usage) = hid_for(code).unwrap();
            assert!(hid::evdev_code(page, usage).is_some(), "{code:?}");
        }
    }

    #[allow(non_snake_case)]
    fn MetaOrSuper() -> KeyCode {
        KeyCode::SuperLeft
    }

    #[test]
    fn letters_follow_the_hid_alphabet() {
        assert_eq!(hid_for(KeyCode::KeyA), Some((7, 0x04)));
        assert_eq!(hid_for(KeyCode::KeyZ), Some((7, 0x1D)));
        assert_eq!(hid_for(KeyCode::Lang1), None);
    }
}
