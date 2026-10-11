//! macOS: `ioreg` and `pmset`.

use std::process::Command;

use crate::{Snapshot, parse};

fn run(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

pub(crate) fn sample() -> Snapshot {
    let (battery_percent, on_ac) = parse::pmset_battery(&run("pmset", &["-g", "batt"]));
    Snapshot {
        idle: parse::ioreg_idle(&run("ioreg", &["-c", "IOHIDSystem", "-d", "4"])),
        locked: parse::ioreg_locked(&run("ioreg", &["-n", "Root", "-d1"])),
        user: std::env::var("USER").ok().filter(|u| !u.is_empty()),
        battery_percent,
        on_ac,
    }
}
