//! Windows input injection through `SendInput`.
//!
//! **Verification status:** type-checked for `x86_64-pc-windows-msvc` only (see docs/STATUS.md).
//!
//! * Keys are sent as **scancodes** (`KEYEVENTF_SCANCODE`), so the *target's* keyboard layout
//!   decides what character appears: exactly like typing at the PC.
//! * Absolute pointer positions use `MOUSEEVENTF_VIRTUALDESK`, which addresses the whole
//!   multi-monitor desktop in a normalised 0..65535 space.
//! * `SendInput` acts on the *calling thread's desktop*. To reach the UAC prompt and the lock
//!   screen the SYSTEM session helper attaches its thread to `Winsta0\Winlogon` first (Phase 6).
//! * Windows does not auto-repeat synthetic held keys, so `Repeat` becomes another key-down.

use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE,
    MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL,
    MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use nexus_proto::wire::input::{
    InputEvent, KeyState, MAX_TEXT_BYTES, PAGE_CONSUMER, PAGE_KEYBOARD, ScrollUnit,
};

use crate::hid;
use crate::{Injector, InputError, MonitorRect};

const XBUTTON1: u32 = 1;
const XBUTTON2: u32 = 2;
/// Wheel units per pixel of smooth scrolling (one 120-unit notch ≈ 40 px of trackpad travel).
const WHEEL_PER_PIXEL: i32 = 3;

/// Injects with `SendInput` on the calling thread's desktop.
#[derive(Debug, Default)]
pub struct WindowsInjector {
    monitors: Vec<MonitorRect>,
}

impl WindowsInjector {
    /// Creates an injector.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

fn send(inputs: &[INPUT]) -> Result<(), InputError> {
    if inputs.is_empty() {
        return Ok(());
    }
    let size = i32::try_from(std::mem::size_of::<INPUT>())
        .map_err(|_| InputError::Failed("INPUT size".into()))?;
    // SAFETY: `inputs` is a valid slice of fully initialised INPUT structures and `size` is
    // `sizeof(INPUT)`, which is what SendInput requires.
    let sent = unsafe { SendInput(inputs, size) };
    if usize::try_from(sent).is_ok_and(|n| n == inputs.len()) {
        Ok(())
    } else {
        // Most often UIPI: the foreground window is elevated and we are not, or the desktop is
        // not the one this thread is attached to.
        Err(InputError::Failed(format!(
            "SendInput injected {sent} of {} events (blocked by UIPI or a secure desktop?)",
            inputs.len()
        )))
    }
}

fn key_input(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn mouse_input(dx: i32, dy: i32, data: u32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Scales a virtual-desktop pixel to SendInput's 0..=65535 absolute range.
fn normalise(value: i32, origin: i32, extent: i32) -> i32 {
    let span = i64::from(extent.max(2) - 1);
    let v = (i64::from(value) - i64::from(origin)).clamp(0, span);
    // Round to nearest so the pointer lands on the intended pixel.
    i32::try_from((v * 65535 + span / 2) / span).unwrap_or(65535)
}

impl Injector for WindowsInjector {
    fn name(&self) -> &'static str {
        "windows-sendinput"
    }

    fn set_monitors(&mut self, monitors: &[MonitorRect]) {
        self.monitors = monitors.to_vec();
    }

    fn inject(&mut self, event: &InputEvent) -> Result<(), InputError> {
        match event {
            InputEvent::MouseMoveAbs { monitor, x, y } => {
                let m = self
                    .monitors
                    .iter()
                    .find(|m| m.index == *monitor)
                    .ok_or_else(|| InputError::Invalid(format!("unknown monitor {monitor}")))?;
                let max_x = i32::try_from(m.width)
                    .unwrap_or(i32::MAX)
                    .saturating_sub(1)
                    .max(0);
                let max_y = i32::try_from(m.height)
                    .unwrap_or(i32::MAX)
                    .saturating_sub(1)
                    .max(0);
                let (ax, ay) = (
                    m.x.saturating_add((*x).clamp(0, max_x)),
                    m.y.saturating_add((*y).clamp(0, max_y)),
                );
                // SAFETY: GetSystemMetrics has no preconditions.
                let (vx, vy, vw, vh) = unsafe {
                    (
                        GetSystemMetrics(SM_XVIRTUALSCREEN),
                        GetSystemMetrics(SM_YVIRTUALSCREEN),
                        GetSystemMetrics(SM_CXVIRTUALSCREEN),
                        GetSystemMetrics(SM_CYVIRTUALSCREEN),
                    )
                };
                let flags = MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK;
                send(&[mouse_input(
                    normalise(ax, vx, vw),
                    normalise(ay, vy, vh),
                    0,
                    flags,
                )])
            }
            InputEvent::MouseMoveRel { dx, dy } => {
                send(&[mouse_input(*dx, *dy, 0, MOUSEEVENTF_MOVE)])
            }
            InputEvent::MouseButton { button, pressed } => {
                let (flags, data) = match (button.0, *pressed) {
                    (0, true) => (MOUSEEVENTF_LEFTDOWN, 0),
                    (0, false) => (MOUSEEVENTF_LEFTUP, 0),
                    (1, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
                    (1, false) => (MOUSEEVENTF_RIGHTUP, 0),
                    (2, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
                    (2, false) => (MOUSEEVENTF_MIDDLEUP, 0),
                    (3, true) => (MOUSEEVENTF_XDOWN, XBUTTON1),
                    (3, false) => (MOUSEEVENTF_XUP, XBUTTON1),
                    (4, true) => (MOUSEEVENTF_XDOWN, XBUTTON2),
                    (4, false) => (MOUSEEVENTF_XUP, XBUTTON2),
                    (other, _) => {
                        return Err(InputError::Unsupported(format!("mouse button {other}")));
                    }
                };
                send(&[mouse_input(0, 0, data, flags)])
            }
            InputEvent::Scroll { dx, dy, unit, .. } => {
                let scale = match unit {
                    ScrollUnit::Detent => 1,
                    ScrollUnit::Pixel => WHEEL_PER_PIXEL,
                };
                let mut inputs = Vec::with_capacity(2);
                if *dy != 0 {
                    inputs.push(mouse_input(
                        0,
                        0,
                        dy.saturating_mul(scale).cast_unsigned(),
                        MOUSEEVENTF_WHEEL,
                    ));
                }
                if *dx != 0 {
                    inputs.push(mouse_input(
                        0,
                        0,
                        dx.saturating_mul(scale).cast_unsigned(),
                        MOUSEEVENTF_HWHEEL,
                    ));
                }
                send(&inputs)
            }
            InputEvent::Key {
                page, usage, state, ..
            } => match *page {
                PAGE_KEYBOARD => {
                    let key = hid::keyboard_key(*usage)
                        .ok_or_else(|| InputError::Unsupported(format!("HID usage {usage:#x}")))?;
                    let mut flags = KEYEVENTF_SCANCODE;
                    if key.win_ext {
                        flags |= KEYEVENTF_EXTENDEDKEY;
                    }
                    if *state == KeyState::Up {
                        flags |= KEYEVENTF_KEYUP;
                    }
                    send(&[key_input(0, u16::from(key.win_scan), flags)])
                }
                PAGE_CONSUMER => {
                    let key = hid::consumer_key(*usage).ok_or_else(|| {
                        InputError::Unsupported(format!("consumer usage {usage:#x}"))
                    })?;
                    let mut flags = KEYEVENTF_EXTENDEDKEY;
                    if *state == KeyState::Up {
                        flags |= KEYEVENTF_KEYUP;
                    }
                    send(&[key_input(u16::from(key.win_vk), 0, flags)])
                }
                other => Err(InputError::Unsupported(format!("HID page {other:#x}"))),
            },
            InputEvent::Text { text } => {
                if text.len() > MAX_TEXT_BYTES {
                    return Err(InputError::Invalid("text too long".into()));
                }
                let mut inputs = Vec::with_capacity(text.len() * 2);
                for unit in text.encode_utf16() {
                    inputs.push(key_input(0, unit, KEYEVENTF_UNICODE));
                    inputs.push(key_input(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
                }
                send(&inputs)
            }
            InputEvent::Touch { .. } | InputEvent::Pen { .. } => Err(InputError::Unsupported(
                "touch and pen injection are not implemented on Windows yet".into(),
            )),
            InputEvent::ReleaseAll => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalisation_maps_the_corners_and_clamps() {
        assert_eq!(normalise(0, 0, 1920), 0);
        assert_eq!(normalise(1919, 0, 1920), 65535);
        assert_eq!(normalise(-5000, 0, 1920), 0);
        assert_eq!(normalise(99999, 0, 1920), 65535);
        // A second monitor to the left makes the virtual desktop start at a negative origin.
        assert_eq!(normalise(-1920, -1920, 3840), 0);
        assert_eq!(normalise(1919, -1920, 3840), 65535);
        let mid = normalise(960, 0, 1920);
        assert!((32_000..=33_600).contains(&mid), "{mid}");
    }
}
