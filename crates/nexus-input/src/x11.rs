//! X11 input injection through the XTest extension.
//!
//! * Keys are sent as **evdev code + 8**, which is the X keycode on every evdev-based server
//!   (Xorg, Xvfb); the *server's* keymap then decides what character results, which is what makes
//!   remote shortcuts follow the target's layout.
//! * Text that the layout cannot type (IME output, emoji) is typed by temporarily binding the
//!   character's keysym to a spare keycode, the same trick `xdotool type` uses.
//! * XTest has no smooth scrolling, touch or pen; those are reported as unsupported (a `uinput`
//!   backend can add them where `/dev/uinput` is writable).

use std::time::Duration;

use nexus_proto::wire::input::{
    InputEvent, KeyState, MAX_TEXT_BYTES, PAGE_CONSUMER, PAGE_KEYBOARD, ScrollUnit,
};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{self, ConnectionExt as _};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use crate::hid;
use crate::scroll::ScrollAccumulator;
use crate::{Injector, InputError, MonitorRect};

const KEY_PRESS: u8 = xproto::KEY_PRESS_EVENT;
const KEY_RELEASE: u8 = xproto::KEY_RELEASE_EVENT;
const BUTTON_PRESS: u8 = xproto::BUTTON_PRESS_EVENT;
const BUTTON_RELEASE: u8 = xproto::BUTTON_RELEASE_EVENT;
const MOTION_NOTIFY: u8 = xproto::MOTION_NOTIFY_EVENT;

/// X pointer button numbers.
const BTN_LEFT: u8 = 1;
const BTN_MIDDLE: u8 = 2;
const BTN_RIGHT: u8 = 3;
const BTN_WHEEL_UP: u8 = 4;
const BTN_WHEEL_DOWN: u8 = 5;
const BTN_WHEEL_LEFT: u8 = 6;
const BTN_WHEEL_RIGHT: u8 = 7;
const BTN_BACK: u8 = 8;
const BTN_FORWARD: u8 = 9;

/// How long applications get to notice a temporary keymap change before the key is pressed.
const MAPPING_SETTLE: Duration = Duration::from_millis(20);
/// Pixels of trackpad travel that count as one wheel notch.
const PIXELS_PER_NOTCH: i32 = 40;
/// Most notches performed for a single scroll event (a runaway delta must not freeze the UI).
const MAX_NOTCHES: i32 = 20;

fn failed(e: impl std::fmt::Display) -> InputError {
    InputError::Failed(e.to_string())
}

/// Injects through XTest on one X display.
pub struct X11Injector {
    conn: RustConnection,
    root: xproto::Window,
    monitors: Vec<MonitorRect>,
    scroll: ScrollAccumulator,
    min_keycode: u8,
    max_keycode: u8,
}

impl std::fmt::Debug for X11Injector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X11Injector").finish_non_exhaustive()
    }
}

impl X11Injector {
    /// Connects to `display`, or `$DISPLAY` when `None`.
    pub fn connect(display: Option<&str>) -> Result<Self, InputError> {
        let name = display
            .map(str::to_owned)
            .or_else(|| std::env::var("DISPLAY").ok());
        let (conn, screen) = RustConnection::connect(name.as_deref()).map_err(|e| {
            InputError::Unavailable(format!(
                "cannot connect to X display {}: {e}",
                name.as_deref().unwrap_or("(DISPLAY unset)")
            ))
        })?;
        let setup = conn.setup();
        let root = setup
            .roots
            .get(screen)
            .map(|s| s.root)
            .ok_or_else(|| InputError::Unavailable("no X screens".into()))?;
        let (min_keycode, max_keycode) = (setup.min_keycode, setup.max_keycode);
        let ver = conn
            .xtest_get_version(2, 2)
            .map_err(failed)?
            .reply()
            .map_err(|_| {
                InputError::Unavailable(
                    "the XTEST extension is not available on this X server".into(),
                )
            })?;
        tracing::debug!(
            major = ver.major_version,
            minor = ver.minor_version,
            "XTest available"
        );
        Ok(Self {
            conn,
            root,
            monitors: Vec::new(),
            scroll: ScrollAccumulator::new(PIXELS_PER_NOTCH),
            min_keycode,
            max_keycode,
        })
    }

    fn fake(&self, kind: u8, detail: u8, x: i16, y: i16) -> Result<(), InputError> {
        self.conn
            .xtest_fake_input(kind, detail, 0, self.root, x, y, 0)
            .map_err(failed)?;
        Ok(())
    }

    fn flush(&self) -> Result<(), InputError> {
        self.conn.flush().map_err(failed)
    }

    fn button(&self, detail: u8, pressed: bool) -> Result<(), InputError> {
        self.fake(
            if pressed {
                BUTTON_PRESS
            } else {
                BUTTON_RELEASE
            },
            detail,
            0,
            0,
        )
    }

    fn click(&self, detail: u8) -> Result<(), InputError> {
        self.button(detail, true)?;
        self.button(detail, false)
    }

    fn monitor(&self, index: u8) -> Result<MonitorRect, InputError> {
        self.monitors
            .iter()
            .find(|m| m.index == index)
            .copied()
            .or_else(|| {
                (self.monitors.is_empty() && index == 0).then_some(MonitorRect {
                    index: 0,
                    x: 0,
                    y: 0,
                    width: u32::MAX / 2,
                    height: u32::MAX / 2,
                    scale: 1.0,
                })
            })
            .ok_or_else(|| InputError::Invalid(format!("unknown monitor {index}")))
    }

    fn key(&self, page: u8, usage: u16, state: KeyState) -> Result<(), InputError> {
        let evdev = hid::evdev_code(page, usage).ok_or_else(|| {
            InputError::Unsupported(format!("HID usage {page:#x}:{usage:#x} has no key code"))
        })?;
        let keycode = u8::try_from(evdev + 8).map_err(|_| {
            InputError::Unsupported(format!("evdev {evdev} is above the X keycode range"))
        })?;
        if keycode < self.min_keycode || keycode > self.max_keycode {
            return Err(InputError::Unsupported(format!(
                "keycode {keycode} outside this server's range"
            )));
        }
        match state {
            KeyState::Down => self.fake(KEY_PRESS, keycode, 0, 0),
            KeyState::Up => self.fake(KEY_RELEASE, keycode, 0, 0),
            // X auto-repeats a held (even synthetic) key by itself (verified by
            // tests/xvfb.rs), so the controller's repeat events must not become extra presses.
            KeyState::Repeat => Ok(()),
        }
    }

    fn scroll_by(&mut self, dx: i32, dy: i32, unit: ScrollUnit) -> Result<(), InputError> {
        let (nx, ny) = self.scroll.push(dx, dy, unit);
        for _ in 0..ny.abs().min(MAX_NOTCHES) {
            self.click(if ny > 0 { BTN_WHEEL_UP } else { BTN_WHEEL_DOWN })?;
        }
        for _ in 0..nx.abs().min(MAX_NOTCHES) {
            self.click(if nx > 0 {
                BTN_WHEEL_RIGHT
            } else {
                BTN_WHEEL_LEFT
            })?;
        }
        Ok(())
    }

    /// Types `text`. Characters the current keymap can produce (unshifted or with Shift) use the
    /// real keys, so ordinary typing causes no keymap churn; anything else (accents the layout
    /// lacks, emoji) is typed by binding its keysym to a spare keycode for an instant.
    fn type_text(&mut self, text: &str) -> Result<(), InputError> {
        if text.len() > MAX_TEXT_BYTES {
            return Err(InputError::Invalid("text too long".into()));
        }
        let count = self.max_keycode - self.min_keycode + 1;
        let map = self
            .conn
            .get_keyboard_mapping(self.min_keycode, count)
            .map_err(failed)?
            .reply()
            .map_err(failed)?;
        let per = usize::from(map.keysyms_per_keycode).max(1);
        let shift =
            u8::try_from(hid::keyboard_key(0xE1).map_or(42, |k| k.evdev) + 8).map_err(failed)?;
        let find = |keysym: u32| -> Option<(u8, bool)> {
            map.keysyms.chunks(per).enumerate().find_map(|(i, syms)| {
                let level = syms.iter().take(2).position(|s| *s == keysym)?;
                Some((self.min_keycode + u8::try_from(i).ok()?, level == 1))
            })
        };
        let mut spare: Option<(u8, Vec<u32>)> = None;
        for ch in text.chars() {
            if ch == '\n' {
                self.key(PAGE_KEYBOARD, 0x28, KeyState::Down)?;
                self.key(PAGE_KEYBOARD, 0x28, KeyState::Up)?;
                continue;
            }
            let keysym = keysym_for(ch);
            if let Some((keycode, shifted)) = find(keysym) {
                if shifted {
                    self.fake(KEY_PRESS, shift, 0, 0)?;
                }
                self.fake(KEY_PRESS, keycode, 0, 0)?;
                self.fake(KEY_RELEASE, keycode, 0, 0)?;
                if shifted {
                    self.fake(KEY_RELEASE, shift, 0, 0)?;
                }
                continue;
            }
            // Not on the keyboard: borrow a spare keycode.
            let (keycode, _) = match &spare {
                Some(s) => s.clone(),
                None => {
                    let keycode = self.spare_keycode()?;
                    let original = self
                        .conn
                        .get_keyboard_mapping(keycode, 1)
                        .map_err(failed)?
                        .reply()
                        .map_err(failed)?
                        .keysyms;
                    spare = Some((keycode, original));
                    (keycode, Vec::new())
                }
            };
            let syms = vec![keysym; per];
            self.conn
                .change_keyboard_mapping(1, keycode, u8::try_from(per).map_err(failed)?, &syms)
                .map_err(failed)?;
            // The server must apply the new mapping before the key press that depends on it.
            self.conn
                .get_input_focus()
                .map_err(failed)?
                .reply()
                .map_err(failed)?;
            // Applications refresh their keymap on MappingNotify; pressing before they have
            // done so types nothing in some toolkits. 20 ms is what xdotool settles on too.
            std::thread::sleep(MAPPING_SETTLE);
            self.fake(KEY_PRESS, keycode, 0, 0)?;
            self.fake(KEY_RELEASE, keycode, 0, 0)?;
            self.flush()?;
            self.conn
                .get_input_focus()
                .map_err(failed)?
                .reply()
                .map_err(failed)?;
            std::thread::sleep(MAPPING_SETTLE / 2);
        }
        if let Some((keycode, original)) = spare {
            self.conn
                .change_keyboard_mapping(1, keycode, u8::try_from(per).map_err(failed)?, &original)
                .map_err(failed)?;
        }
        self.flush()
    }

    /// A keycode with no keysyms bound, or the last keycode if every one is used.
    fn spare_keycode(&self) -> Result<u8, InputError> {
        let count = self.max_keycode - self.min_keycode + 1;
        let map = self
            .conn
            .get_keyboard_mapping(self.min_keycode, count)
            .map_err(failed)?
            .reply()
            .map_err(failed)?;
        let per = usize::from(map.keysyms_per_keycode).max(1);
        for (i, chunk) in map.keysyms.chunks(per).enumerate().rev() {
            if chunk.iter().all(|s| *s == 0) {
                return u8::try_from(i)
                    .map(|i| self.min_keycode + i)
                    .map_err(failed);
            }
        }
        Ok(self.max_keycode)
    }
}

/// X keysym for a Unicode character.
fn keysym_for(ch: char) -> u32 {
    let cp = u32::from(ch);
    // Latin-1 keysyms equal their code points; everything else is 0x0100_0000 + code point.
    if (0x20..=0x7E).contains(&cp) || (0xA0..=0xFF).contains(&cp) {
        cp
    } else {
        0x0100_0000 + cp
    }
}

impl Injector for X11Injector {
    fn name(&self) -> &'static str {
        "x11-xtest"
    }

    fn set_monitors(&mut self, monitors: &[MonitorRect]) {
        self.monitors = monitors.to_vec();
    }

    fn inject(&mut self, event: &InputEvent) -> Result<(), InputError> {
        let result = match event {
            InputEvent::MouseMoveAbs { monitor, x, y } => {
                let m = self.monitor(*monitor)?;
                let max_x = i32::try_from(m.width)
                    .unwrap_or(i32::MAX)
                    .saturating_sub(1)
                    .max(0);
                let max_y = i32::try_from(m.height)
                    .unwrap_or(i32::MAX)
                    .saturating_sub(1)
                    .max(0);
                let ax = m.x.saturating_add((*x).clamp(0, max_x));
                let ay = m.y.saturating_add((*y).clamp(0, max_y));
                let ax =
                    i16::try_from(ax).map_err(|_| InputError::Invalid("x out of range".into()))?;
                let ay =
                    i16::try_from(ay).map_err(|_| InputError::Invalid("y out of range".into()))?;
                self.fake(MOTION_NOTIFY, 0, ax, ay)
            }
            InputEvent::MouseMoveRel { dx, dy } => {
                let dx = i16::try_from((*dx).clamp(-30_000, 30_000)).unwrap_or(0);
                let dy = i16::try_from((*dy).clamp(-30_000, 30_000)).unwrap_or(0);
                self.fake(MOTION_NOTIFY, 1, dx, dy)
            }
            InputEvent::MouseButton { button, pressed } => {
                let detail = match button.0 {
                    0 => BTN_LEFT,
                    1 => BTN_RIGHT,
                    2 => BTN_MIDDLE,
                    3 => BTN_BACK,
                    4 => BTN_FORWARD,
                    other => return Err(InputError::Unsupported(format!("mouse button {other}"))),
                };
                self.button(detail, *pressed)
            }
            InputEvent::Scroll { dx, dy, unit, .. } => self.scroll_by(*dx, *dy, *unit),
            InputEvent::Key {
                page, usage, state, ..
            } => {
                if *page != PAGE_KEYBOARD && *page != PAGE_CONSUMER {
                    return Err(InputError::Unsupported(format!("HID page {page:#x}")));
                }
                self.key(*page, *usage, *state)
            }
            InputEvent::Text { text } => self.type_text(text),
            InputEvent::Touch { .. } | InputEvent::Pen { .. } => {
                return Err(InputError::Unsupported(
                    "touch and pen need uinput; XTest cannot express them".into(),
                ));
            }
            // Handled by `Guarded`, which knows what is held.
            InputEvent::ReleaseAll => Ok(()),
        };
        result?;
        self.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keysyms() {
        assert_eq!(keysym_for('a'), 0x61);
        assert_eq!(keysym_for('é'), 0xE9);
        assert_eq!(keysym_for('€'), 0x0100_20AC);
        assert_eq!(keysym_for('😀'), 0x0101_F600);
    }
}
