//! Windows: `GetLastInputInfo`, the input desktop (locked screens switch it), power status.

use std::time::Duration;

use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, OpenInputDesktop,
};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

use crate::Snapshot;

pub(crate) fn sample() -> Snapshot {
    let (battery_percent, on_ac) = power();
    Snapshot {
        idle: idle(),
        locked: locked(),
        user: std::env::var("USERNAME").ok().filter(|u| !u.is_empty()),
        battery_percent,
        on_ac,
    }
}

fn idle() -> Option<Duration> {
    let mut info = LASTINPUTINFO {
        cbSize: u32::try_from(std::mem::size_of::<LASTINPUTINFO>()).ok()?,
        dwTime: 0,
    };
    // SAFETY: `info` is a properly sized, initialised LASTINPUTINFO that outlives the call.
    let ok = unsafe { GetLastInputInfo(&mut info) }.as_bool();
    if !ok {
        return None;
    }
    // SAFETY: GetTickCount takes no arguments and has no preconditions.
    let now = unsafe { GetTickCount() };
    // Both are 32-bit millisecond counters that wrap together.
    Some(Duration::from_millis(u64::from(
        now.wrapping_sub(info.dwTime),
    )))
}

/// When the secure desktop (lock screen, UAC) is up, a normal process cannot open the input
/// desktop with switch rights: that failure is the "locked" signal.
fn locked() -> bool {
    // SAFETY: plain query; the handle, if any, is closed right below.
    match unsafe {
        OpenInputDesktop(
            DESKTOP_CONTROL_FLAGS(0),
            false,
            DESKTOP_ACCESS_FLAGS(0x0100), // DESKTOP_SWITCHDESKTOP
        )
    } {
        Ok(desktop) => {
            // SAFETY: `desktop` was just returned by OpenInputDesktop and is not used again.
            let _ = unsafe { CloseDesktop(desktop) };
            false
        }
        Err(_) => true,
    }
}

fn power() -> (Option<u8>, bool) {
    let mut status = SYSTEM_POWER_STATUS::default();
    // SAFETY: `status` is a valid out-parameter for the duration of the call.
    if unsafe { GetSystemPowerStatus(&mut status) }.is_err() {
        return (None, true);
    }
    let percent = (status.BatteryLifePercent != 255).then_some(status.BatteryLifePercent);
    (percent.filter(|p| *p <= 100), status.ACLineStatus != 0)
}
