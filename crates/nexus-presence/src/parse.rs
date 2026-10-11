//! Parsers for the text the operating system's own tools print. Pure functions, so they are
//! tested on every platform with captured samples.

use std::time::Duration;

/// `HIDIdleTime` (nanoseconds) from `ioreg -c IOHIDSystem`.
#[must_use]
pub fn ioreg_idle(text: &str) -> Option<Duration> {
    let line = text.lines().find(|l| l.contains("HIDIdleTime"))?;
    let digits: String = line
        .rsplit('=')
        .next()?
        .chars()
        .filter(char::is_ascii_digit)
        .collect();
    let ns: u64 = digits.parse().ok()?;
    Some(Duration::from_nanos(ns))
}

/// Is `CGSSessionScreenIsLocked` set in `ioreg -n Root -d1` output?
#[must_use]
pub fn ioreg_locked(text: &str) -> bool {
    text.lines()
        .find(|l| l.contains("CGSSessionScreenIsLocked"))
        .is_some_and(|l| l.contains("Yes") || l.contains("true") || l.trim_end().ends_with('1'))
}

/// `(percent, on_ac)` from `pmset -g batt`.
#[must_use]
pub fn pmset_battery(text: &str) -> (Option<u8>, bool) {
    let on_ac = !text.contains("Battery Power");
    let percent = text.lines().find_map(|l| {
        let end = l.find('%')?;
        let start = l[..end]
            .rfind(|c: char| !c.is_ascii_digit())
            .map_or(0, |i| i + 1);
        l[start..end].parse::<u8>().ok().filter(|p| *p <= 100)
    });
    (percent, on_ac)
}

/// `LockedHint=yes` from `loginctl show-session -p LockedHint`.
#[must_use]
pub fn loginctl_locked(text: &str) -> bool {
    text.lines().any(|l| {
        l.trim().eq_ignore_ascii_case("LockedHint=yes") || l.trim().eq_ignore_ascii_case("yes")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioreg_idle_time_is_read_in_nanoseconds() {
        let sample = "    | |   \"HIDIdleTime\" = 12500000000\n    | |   \"Other\" = 1\n";
        assert_eq!(ioreg_idle(sample), Some(Duration::from_millis(12_500)));
        assert_eq!(ioreg_idle("nothing here"), None);
        assert_eq!(ioreg_idle("\"HIDIdleTime\" = oops"), None);
    }

    #[test]
    fn the_lock_flag_is_recognised() {
        assert!(ioreg_locked("  \"CGSSessionScreenIsLocked\" = Yes\n"));
        assert!(!ioreg_locked("  \"CGSSessionScreenIsLocked\" = No\n"));
        assert!(
            !ioreg_locked("  \"SomethingElse\" = Yes\n"),
            "other keys do not count"
        );
        assert!(!ioreg_locked(""));
    }

    #[test]
    fn battery_text_parses() {
        let on_battery = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1)\t87%; discharging; 4:12 remaining present: true\n";
        assert_eq!(pmset_battery(on_battery), (Some(87), false));
        let on_ac = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t100%; charged; 0:00 remaining present: true\n";
        assert_eq!(pmset_battery(on_ac), (Some(100), true));
        let desktop = "Now drawing from 'AC Power'\n";
        assert_eq!(pmset_battery(desktop), (None, true));
    }

    #[test]
    fn logind_lock_hint_parses() {
        assert!(loginctl_locked("LockedHint=yes\n"));
        assert!(loginctl_locked("yes\n"));
        assert!(!loginctl_locked("LockedHint=no\n"));
        assert!(!loginctl_locked(""));
    }
}
