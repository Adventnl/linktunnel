//! Feature grants: what a paired controller may do on a target.
//!
//! Stored as strings in pairing records so new capabilities never need a schema change.
//! Presence visibility is implied by any pairing and is not a grant.

/// View the screen.
pub const SCREEN: &str = "screen";
/// Send keyboard/mouse/touch input.
pub const INPUT: &str = "input";
/// Clipboard sync.
pub const CLIPBOARD: &str = "clipboard";
/// File transfer and the file panel.
pub const FILES: &str = "files";
/// Terminal sessions and port forwarding.
pub const TERMINAL: &str = "terminal";
/// Lock, sleep, restart, shutdown.
pub const POWER: &str = "power";
/// Wake-on-LAN requests.
pub const WAKE: &str = "wake";

/// Everything: the default between two devices owned by the same person.
#[must_use]
pub fn all() -> Vec<String> {
    [SCREEN, INPUT, CLIPBOARD, FILES, TERMINAL, POWER, WAKE]
        .iter()
        .map(ToString::to_string)
        .collect()
}

/// A grant string is short lowercase ASCII (`screen`, `files.read`, …).
#[must_use]
pub fn is_valid(grant: &str) -> bool {
    !grant.is_empty()
        && grant.len() <= 32
        && grant.starts_with(|c: char| c.is_ascii_lowercase())
        && grant
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_unique() {
        let all = all();
        assert!(all.iter().all(|g| is_valid(g)));
        let mut sorted = all.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), all.len());
    }

    #[test]
    fn validation() {
        for ok in ["screen", "files.read", "a1_b-c"] {
            assert!(is_valid(ok), "{ok}");
        }
        for bad in ["", "Screen", "1abc", "has space", "ü", &"a".repeat(33)] {
            assert!(!is_valid(bad), "{bad}");
        }
    }
}
