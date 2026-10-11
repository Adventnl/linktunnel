//! Human key names → HID usages, for the CLI (`nexus key mac "cmd+shift+4"`) and automation.

use crate::InputError;
use crate::hid;

/// A parsed key chord: modifiers first, then the final key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    /// Keyboard-page usages, in press order (modifiers, then the key).
    pub usages: Vec<u16>,
}

fn alias(name: &str) -> Option<u16> {
    Some(match name {
        "ctrl" | "control" | "lctrl" => 0xE0,
        "shift" | "lshift" => 0xE1,
        "alt" | "option" | "opt" | "lalt" => 0xE2,
        "meta" | "cmd" | "command" | "win" | "windows" | "super" | "lmeta" => 0xE3,
        "rctrl" => 0xE4,
        "rshift" => 0xE5,
        "ralt" | "altgr" => 0xE6,
        "rmeta" | "rcmd" => 0xE7,
        "enter" | "return" => 0x28,
        "esc" | "escape" => 0x29,
        "backspace" | "bksp" => 0x2A,
        "tab" => 0x2B,
        "space" | "spacebar" => 0x2C,
        "minus" | "-" => 0x2D,
        "equal" | "equals" | "=" => 0x2E,
        "lbracket" | "[" => 0x2F,
        "rbracket" | "]" => 0x30,
        "backslash" | "\\" => 0x31,
        "semicolon" | ";" => 0x33,
        "quote" | "'" => 0x34,
        "grave" | "`" | "backtick" => 0x35,
        "comma" | "," => 0x36,
        "period" | "dot" | "." => 0x37,
        "slash" | "/" => 0x38,
        "capslock" | "caps" => 0x39,
        "printscreen" | "prtsc" => 0x46,
        "scrolllock" => 0x47,
        "pause" => 0x48,
        "insert" | "ins" => 0x49,
        "home" => 0x4A,
        "pageup" | "pgup" => 0x4B,
        "delete" | "del" => 0x4C,
        "end" => 0x4D,
        "pagedown" | "pgdn" => 0x4E,
        "right" => 0x4F,
        "left" => 0x50,
        "down" => 0x51,
        "up" => 0x52,
        "numlock" => 0x53,
        "menu" | "contextmenu" => 0x65,
        _ => return None,
    })
}

/// Looks up a single key by name (`a`, `7`, `f5`, `enter`, `cmd`, …).
pub fn key_by_name(name: &str) -> Result<u16, InputError> {
    let lower = name.trim().to_lowercase();
    if let Some(u) = alias(&lower) {
        return Ok(u);
    }
    hid::all_keyboard_keys()
        .iter()
        .find(|k| k.name.eq_ignore_ascii_case(&lower))
        .map(|k| k.usage)
        .ok_or_else(|| InputError::Invalid(format!("unknown key '{name}'")))
}

/// Parses `"ctrl+shift+t"`-style chords. The last element is the key, the rest modifiers.
pub fn parse_chord(text: &str) -> Result<Chord, InputError> {
    let parts: Vec<&str> = text.split('+').filter(|p| !p.is_empty()).collect();
    // "+" itself is spelled "plus"; a trailing '+' would otherwise vanish in the split.
    if parts.is_empty() {
        return Err(InputError::Invalid("empty key chord".into()));
    }
    let mut usages = Vec::with_capacity(parts.len());
    for part in parts {
        let u = if part.eq_ignore_ascii_case("plus") {
            0x2E
        } else {
            key_by_name(part)?
        };
        // "plus" needs shift on a US layout.
        if part.eq_ignore_ascii_case("plus") {
            usages.push(0xE1);
        }
        usages.push(u);
    }
    Ok(Chord { usages })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_aliases_and_chords() {
        assert_eq!(key_by_name("a").unwrap(), 0x04);
        assert_eq!(key_by_name("A").unwrap(), 0x04);
        assert_eq!(key_by_name("7").unwrap(), 0x24);
        assert_eq!(key_by_name("F5").unwrap(), 0x3E);
        assert_eq!(key_by_name("Return").unwrap(), 0x28);
        assert_eq!(key_by_name("cmd").unwrap(), 0xE3);
        assert_eq!(
            parse_chord("ctrl+shift+t").unwrap().usages,
            vec![0xE0, 0xE1, 0x17]
        );
        assert_eq!(parse_chord("alt+f4").unwrap().usages, vec![0xE2, 0x3D]);
        assert_eq!(parse_chord("enter").unwrap().usages, vec![0x28]);
    }

    #[test]
    fn errors() {
        assert!(key_by_name("hyperspace").is_err());
        assert!(parse_chord("").is_err());
        assert!(parse_chord("ctrl+nope").is_err());
    }

    #[test]
    fn every_table_key_is_reachable_by_its_own_name() {
        for k in hid::all_keyboard_keys() {
            assert_eq!(
                key_by_name(k.name).unwrap(),
                alias(&k.name.to_lowercase()).unwrap_or(k.usage),
                "{}",
                k.name
            );
        }
    }
}
