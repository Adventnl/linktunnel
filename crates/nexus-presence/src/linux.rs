//! Linux: X11 screensaver idle time, logind lock hint, sysfs battery.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::Snapshot;

pub(crate) fn sample() -> Snapshot {
    let (battery_percent, on_ac) = battery(Path::new("/sys/class/power_supply"));
    Snapshot {
        idle: x11_idle(),
        locked: locked(),
        user: std::env::var("USER").ok().filter(|u| !u.is_empty()),
        battery_percent,
        on_ac,
    }
}

fn x11_idle() -> Option<Duration> {
    use x11rb::connection::Connection;
    use x11rb::protocol::screensaver::ConnectionExt as _;
    // Only when this process is attached to a display (a headless server has none).
    std::env::var_os("DISPLAY")?;
    let (conn, screen) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots.get(screen)?.root;
    let info = conn.screensaver_query_info(root).ok()?.reply().ok()?;
    Some(Duration::from_millis(u64::from(info.ms_since_user_input)))
}

fn locked() -> bool {
    let Ok(session) = std::env::var("XDG_SESSION_ID") else {
        return false;
    };
    Command::new("loginctl")
        .args(["show-session", &session, "-p", "LockedHint", "--value"])
        .output()
        .ok()
        .is_some_and(|o| crate::parse::loginctl_locked(&String::from_utf8_lossy(&o.stdout)))
}

/// `(percent, on_ac)` from `/sys/class/power_supply` laid out under `root`.
fn battery(root: &Path) -> (Option<u8>, bool) {
    let mut percent = None;
    let mut on_ac = true;
    let mut has_battery = false;
    if let Ok(rd) = std::fs::read_dir(root) {
        for entry in rd.flatten() {
            let dir = entry.path();
            let kind = std::fs::read_to_string(dir.join("type")).unwrap_or_default();
            match kind.trim() {
                "Battery" => {
                    has_battery = true;
                    percent = std::fs::read_to_string(dir.join("capacity"))
                        .ok()
                        .and_then(|c| c.trim().parse::<u8>().ok())
                        .filter(|p| *p <= 100);
                    let status = std::fs::read_to_string(dir.join("status")).unwrap_or_default();
                    if status.trim() == "Discharging" {
                        on_ac = false;
                    }
                }
                "Mains"
                    if has_battery
                        && std::fs::read_to_string(dir.join("online"))
                            .is_ok_and(|v| v.trim() == "0") =>
                {
                    on_ac = false;
                }
                _ => {}
            }
        }
    }
    (percent, on_ac)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sysfs_battery_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let bat = dir.path().join("BAT0");
        std::fs::create_dir(&bat).unwrap();
        std::fs::write(bat.join("type"), "Battery\n").unwrap();
        std::fs::write(bat.join("capacity"), "63\n").unwrap();
        std::fs::write(bat.join("status"), "Discharging\n").unwrap();
        assert_eq!(battery(dir.path()), (Some(63), false));
        std::fs::write(bat.join("status"), "Charging\n").unwrap();
        assert_eq!(battery(dir.path()), (Some(63), true));
        assert_eq!(battery(&dir.path().join("missing")), (None, true));
    }
}
