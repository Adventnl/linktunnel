//! Windows input injection through `SendInput`.
//!
//! **Verification status:** source-only Windows revision; no builds or tests (docs/STATUS.md).
//!
//! * Keys are sent as **scancodes** (`KEYEVENTF_SCANCODE`), so the *target's* keyboard layout
//!   decides what character appears: exactly like typing at the PC.
//! * Absolute pointer positions use `MOUSEEVENTF_VIRTUALDESK`, which addresses the whole
//!   multi-monitor desktop in a normalised 0..65535 space.
//! * `SendInput` acts on the *calling thread's desktop*. Before each event the injector re-attaches
//!   its thread to the current input desktop (`nexus-desktop`), so when the UAC prompt or the lock
//!   screen appears the next keystroke lands there. That only works for a SYSTEM process in the
//!   session (see `nexus-winservice`); for an ordinary user input is refused while an inaccessible
//!   secure desktop is active, and resumes after returning to Default.
//! * Windows does not auto-repeat synthetic held keys, so `Repeat` becomes another key-down.

use super::windows_pointer::Pointers;
use std::collections::BTreeSet;
use std::sync::OnceLock;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::System::Threading::{EVENT_MODIFY_STATE, OpenEventW, SetEvent};
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
use windows::core::{PCWSTR, w};

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
    desktop: nexus_desktop::ThreadDesktop,
    keys: BTreeSet<(u8, u16)>,
    buttons: BTreeSet<u8>,
    pointers: Pointers,
    selected_monitor: u8,
}

impl WindowsInjector {
    /// Creates an injector.
    #[must_use]
    pub fn new() -> Self {
        if let Err(e) = nexus_desktop::enable_physical_coordinates() {
            tracing::warn!(error = %e, "could not enable physical pointer coordinates");
        }
        Self::default()
    }

    /// Every event checks the desktop; a click must never be delivered to a stale UAC/login
    /// desktop. Release held state on the old desktop before changing the thread attachment.
    fn follow_desktop(&mut self) -> Result<(), InputError> {
        let input =
            nexus_desktop::input_desktop_name().map_err(|e| InputError::Failed(e.to_string()))?;
        if self.desktop.attached().is_some_and(|name| name != input) {
            // The old desktop may already have disappeared. Best-effort release must not
            // prevent attaching to Winlogon; stale held state never carries into the new desktop.
            let _ = self.release_native();
            self.keys.clear();
            self.buttons.clear();
            self.pointers = Pointers::default();
        }
        self.desktop
            .sync()
            .map_err(|e| InputError::Failed(e.to_string()))?;
        Ok(())
    }

    fn release_native(&mut self) -> Result<(), InputError> {
        let mut first = None;
        for (page, usage) in self.keys.clone() {
            if let Err(e) = send_key(page, usage, KeyState::Up) {
                first.get_or_insert(e);
            } else {
                self.keys.remove(&(page, usage));
            }
        }
        for button in self.buttons.clone() {
            if let Err(e) = send_button(button, false) {
                first.get_or_insert(e);
            } else {
                self.buttons.remove(&button);
            }
        }
        if let Err(e) = self.pointers.release_all() {
            first.get_or_insert(e);
        }
        first.map_or(Ok(()), Err)
    }

    fn pointer_position(&self, x: i32, y: i32) -> Result<(i32, i32), InputError> {
        let monitor = self
            .monitors
            .iter()
            .find(|m| m.index == self.selected_monitor)
            .or_else(|| self.monitors.first())
            .ok_or_else(|| InputError::Invalid("no pointer monitor".into()))?;
        Ok((
            monitor
                .x
                .saturating_add(x.clamp(0, monitor.width.saturating_sub(1) as i32)),
            monitor
                .y
                .saturating_add(y.clamp(0, monitor.height.saturating_sub(1) as i32)),
        ))
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
    fn release_native_state(&mut self) -> Result<(), InputError> {
        self.release_native()
    }

    fn name(&self) -> &'static str {
        "windows-sendinput"
    }

    fn set_monitors(&mut self, monitors: &[MonitorRect]) {
        self.monitors = monitors.to_vec();
    }

    fn inject(&mut self, event: &InputEvent) -> Result<(), InputError> {
        self.follow_desktop()?;
        match event {
            InputEvent::MouseMoveAbs { monitor, x, y } => {
                self.selected_monitor = *monitor;
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
                send_button(button.0, *pressed)?;
                if *pressed {
                    self.buttons.insert(button.0);
                } else {
                    self.buttons.remove(&button.0);
                }
                Ok(())
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
            } => {
                // Ctrl+Alt+Del is a secure attention sequence, never a SendInput chord.
                if *page == PAGE_KEYBOARD
                    && *usage == 0x4c
                    && *state == KeyState::Down
                    && [0xe0, 0xe4]
                        .iter()
                        .any(|u| self.keys.contains(&(PAGE_KEYBOARD, *u)))
                    && [0xe2, 0xe6]
                        .iter()
                        .any(|u| self.keys.contains(&(PAGE_KEYBOARD, *u)))
                {
                    self.release_native()?;
                    return request_secure_attention();
                }
                send_key(*page, *usage, *state)?;
                if *state == KeyState::Up {
                    self.keys.remove(&(*page, *usage));
                } else {
                    self.keys.insert((*page, *usage));
                }
                Ok(())
            }
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
            InputEvent::Touch {
                id,
                phase,
                x,
                y,
                pressure,
            } => {
                let (x, y) = self.pointer_position(*x, *y)?;
                self.pointers.touch(*id, *phase, x, y, *pressure)
            }
            InputEvent::Pen {
                id,
                phase,
                x,
                y,
                pressure,
                tilt_x,
                tilt_y,
                buttons,
            } => {
                let (x, y) = self.pointer_position(*x, *y)?;
                self.pointers
                    .pen(*id, *phase, x, y, *pressure, *tilt_x, *tilt_y, *buttons)
            }
            InputEvent::ReleaseAll => self.release_native(),
        }
    }
}

impl Drop for WindowsInjector {
    fn drop(&mut self) {
        let _ = self.release_native();
    }
}

static SAS_EVENT: OnceLock<String> = OnceLock::new();

/// Sets the service-created SYSTEM-only event name. Called only by the session child at startup.
pub fn configure_secure_attention(name: String) -> Result<(), InputError> {
    if !name.starts_with(r"Global\NexusSas-") || name.len() != r"Global\NexusSas-".len() + 32 {
        return Err(InputError::Invalid(
            "invalid secure-attention broker name".into(),
        ));
    }
    SAS_EVENT
        .set(name)
        .map_err(|_| InputError::Invalid("secure-attention broker already configured".into()))
}

/// Whether this process is the service-launched session child with a secure-attention broker.
#[must_use]
pub fn secure_attention_available() -> bool {
    SAS_EVENT.get().is_some()
}

fn request_secure_attention() -> Result<(), InputError> {
    let name = SAS_EVENT.get().ok_or_else(|| {
        InputError::Unsupported(
            "Ctrl+Alt+Del requires the Nexus Windows service and software SAS policy".into(),
        )
    })?;
    let mut policy = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: constant registry names, DWORD output with its exact size. The application never
    // changes this policy at runtime; local/domain administrators remain in control of it.
    let read = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Policies\\System"),
            w!("SoftwareSASGeneration"),
            RRF_RT_REG_DWORD,
            None,
            Some(std::ptr::from_mut(&mut policy).cast()),
            Some(&mut size),
        )
    };
    if read.0 != 0 || !matches!(policy, 1 | 3) {
        return Err(InputError::Unsupported(
            "Windows software SAS policy must allow Services".into(),
        ));
    }
    let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: terminated event name; only EVENT_MODIFY_STATE is requested, no inheritance.
    let event = unsafe { OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(name.as_ptr())) }
        .map_err(|e| InputError::Failed(format!("open secure-attention broker: {e}")))?;
    // SAFETY: event is live and has EVENT_MODIFY_STATE access.
    let result = unsafe { SetEvent(event) }.map_err(|e| InputError::Failed(e.to_string()));
    // SAFETY: opened handle, used no further and closed once.
    let _ = unsafe { CloseHandle(event) };
    result
}

fn send_key(page: u8, usage: u16, state: KeyState) -> Result<(), InputError> {
    let up = state == KeyState::Up;
    let (vk, scan, mut flags) = match page {
        PAGE_KEYBOARD => {
            let key = hid::keyboard_key(usage)
                .ok_or_else(|| InputError::Unsupported(format!("HID usage {usage:#x}")))?;
            // Pause uses an E1 sequence that cannot be represented by the E0 SendInput flag.
            if usage == 0x48 {
                (0x13, 0, KEYBD_EVENT_FLAGS(0))
            } else {
                (
                    0,
                    u16::from(key.win_scan),
                    if key.win_ext {
                        KEYEVENTF_SCANCODE | KEYEVENTF_EXTENDEDKEY
                    } else {
                        KEYEVENTF_SCANCODE
                    },
                )
            }
        }
        PAGE_CONSUMER => {
            let key = hid::consumer_key(usage)
                .ok_or_else(|| InputError::Unsupported(format!("consumer usage {usage:#x}")))?;
            (u16::from(key.win_vk), 0, KEYEVENTF_EXTENDEDKEY)
        }
        other => return Err(InputError::Unsupported(format!("HID page {other:#x}"))),
    };
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    send(&[key_input(vk, scan, flags)])
}

fn send_button(button: u8, pressed: bool) -> Result<(), InputError> {
    let (flags, data) = match (button, pressed) {
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
        (other, _) => return Err(InputError::Unsupported(format!("mouse button {other}"))),
    };
    send(&[mouse_input(0, 0, data, flags)])
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
