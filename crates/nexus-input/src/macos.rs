//! macOS input injection with `CGEvent`.
//!
//! **Verification status:** type-checked for `aarch64-apple-darwin`; not run here (docs/STATUS.md).
//!
//! * Needs the **Accessibility** permission (System Settings → Privacy & Security). Without it
//!   `CGEventPost` silently does nothing, so [`MacInjector::new`] checks `AXIsProcessTrusted`
//!   up front and reports a clear error.
//! * Coordinates: pointer positions arrive in device pixels and are converted to the points
//!   `CGEvent` uses via the monitor's `scale` (2.0 on Retina).
//! * Modifier state is tracked and stamped on every event, so ⌘C, ⌥-click, Shift-drag work.
//! * Secure Event Input (password fields) suppresses synthetic keystrokes; that is a macOS
//!   guarantee, documented in docs/LIMITATIONS.md.

use std::time::{Duration, Instant};

use nexus_proto::wire::input::{InputEvent, KeyState, MAX_TEXT_BYTES, PAGE_KEYBOARD, ScrollUnit};
use objc2_core_foundation::{CFRetained, CGPoint};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation,
    CGEventType, CGMouseButton, CGScrollEventUnit,
};

use crate::hid;
use crate::{Injector, InputError, MonitorRect};

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

/// Whether this process may post synthetic input.
#[must_use]
pub fn accessibility_granted() -> bool {
    // SAFETY: AXIsProcessTrusted takes no arguments and has no preconditions.
    unsafe { AXIsProcessTrusted() }
}

/// Maximum gap between clicks that counts as a double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(450);

/// Posts `CGEvent`s.
pub struct MacInjector {
    source: CFRetained<CGEventSource>,
    monitors: Vec<MonitorRect>,
    pos: CGPoint,
    held_buttons: [bool; 3],
    flags: CGEventFlags,
    last_click: Option<(Instant, u8, i64)>,
}

impl std::fmt::Debug for MacInjector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MacInjector").finish_non_exhaustive()
    }
}

// SAFETY: the CGEventSource is only used through `&mut self`, from one thread at a time.
unsafe impl Send for MacInjector {}

impl MacInjector {
    /// Creates an injector, or explains which permission is missing.
    pub fn new() -> Result<Self, InputError> {
        if !accessibility_granted() {
            return Err(InputError::Permission(
                "grant Nexus the Accessibility permission in System Settings → Privacy & Security → Accessibility".into(),
            ));
        }
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .ok_or_else(|| InputError::Unavailable("CGEventSourceCreate failed".into()))?;
        Ok(Self {
            source,
            monitors: Vec::new(),
            pos: CGPoint { x: 0.0, y: 0.0 },
            held_buttons: [false; 3],
            flags: CGEventFlags::empty(),
            last_click: None,
        })
    }

    fn post(&self, event: Option<CFRetained<CGEvent>>) -> Result<(), InputError> {
        let event = event.ok_or_else(|| InputError::Failed("CGEvent creation failed".into()))?;
        CGEvent::set_flags(Some(&event), self.flags);
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
        Ok(())
    }

    fn mouse_event(
        &self,
        kind: CGEventType,
        button: CGMouseButton,
        click_state: i64,
    ) -> Result<(), InputError> {
        let event = CGEvent::new_mouse_event(Some(&self.source), kind, self.pos, button);
        if let Some(e) = &event
            && click_state > 0
        {
            CGEvent::set_integer_value_field(
                Some(e),
                CGEventField::MouseEventClickState,
                click_state,
            );
        }
        self.post(event)
    }

    fn modifier_flag(keycode: u16) -> Option<CGEventFlags> {
        Some(match keycode {
            0x38 | 0x3C => CGEventFlags::MaskShift,
            0x3B | 0x3E => CGEventFlags::MaskControl,
            0x3A | 0x3D => CGEventFlags::MaskAlternate,
            0x37 | 0x36 => CGEventFlags::MaskCommand,
            0x39 => CGEventFlags::MaskAlphaShift,
            _ => return None,
        })
    }

    fn move_to(&mut self, x: f64, y: f64) -> Result<(), InputError> {
        self.pos = CGPoint { x, y };
        let (kind, button) = if self.held_buttons[0] {
            (CGEventType::LeftMouseDragged, CGMouseButton::Left)
        } else if self.held_buttons[1] {
            (CGEventType::RightMouseDragged, CGMouseButton::Right)
        } else if self.held_buttons[2] {
            (CGEventType::OtherMouseDragged, CGMouseButton::Center)
        } else {
            (CGEventType::MouseMoved, CGMouseButton::Left)
        };
        self.mouse_event(kind, button, 0)
    }
}

impl Injector for MacInjector {
    fn name(&self) -> &'static str {
        "macos-cgevent"
    }

    fn set_monitors(&mut self, monitors: &[MonitorRect]) {
        self.monitors = monitors.to_vec();
    }

    fn inject(&mut self, event: &InputEvent) -> Result<(), InputError> {
        match event {
            InputEvent::MouseMoveAbs { monitor, x, y } => {
                let m = *self
                    .monitors
                    .iter()
                    .find(|m| m.index == *monitor)
                    .ok_or_else(|| InputError::Invalid(format!("unknown monitor {monitor}")))?;
                let scale = f64::from(if m.scale > 0.0 { m.scale } else { 1.0 });
                let max_x = i32::try_from(m.width)
                    .unwrap_or(i32::MAX)
                    .saturating_sub(1)
                    .max(0);
                let max_y = i32::try_from(m.height)
                    .unwrap_or(i32::MAX)
                    .saturating_sub(1)
                    .max(0);
                let px = f64::from((*x).clamp(0, max_x)) / scale + f64::from(m.x);
                let py = f64::from((*y).clamp(0, max_y)) / scale + f64::from(m.y);
                self.move_to(px, py)
            }
            InputEvent::MouseMoveRel { dx, dy } => {
                let (x, y) = (self.pos.x + f64::from(*dx), self.pos.y + f64::from(*dy));
                self.move_to(x, y)
            }
            InputEvent::MouseButton { button, pressed } => {
                let (down, up, cg, slot) = match button.0 {
                    0 => (
                        CGEventType::LeftMouseDown,
                        CGEventType::LeftMouseUp,
                        CGMouseButton::Left,
                        0,
                    ),
                    1 => (
                        CGEventType::RightMouseDown,
                        CGEventType::RightMouseUp,
                        CGMouseButton::Right,
                        1,
                    ),
                    n @ 2..=4 => (
                        CGEventType::OtherMouseDown,
                        CGEventType::OtherMouseUp,
                        CGMouseButton(u32::from(n)),
                        2,
                    ),
                    other => return Err(InputError::Unsupported(format!("mouse button {other}"))),
                };
                self.held_buttons[slot] = *pressed;
                let click_state = if *pressed {
                    let now = Instant::now();
                    let count = match self.last_click {
                        Some((t, b, c))
                            if b == button.0 && now.duration_since(t) < DOUBLE_CLICK =>
                        {
                            c + 1
                        }
                        _ => 1,
                    };
                    self.last_click = Some((now, button.0, count));
                    count
                } else {
                    self.last_click.map_or(1, |l| l.2)
                };
                self.mouse_event(if *pressed { down } else { up }, cg, click_state)
            }
            InputEvent::Scroll { dx, dy, unit, .. } => {
                let (units, dy, dx) = match unit {
                    ScrollUnit::Detent => (CGScrollEventUnit::Line, dy / 120, dx / 120),
                    ScrollUnit::Pixel => (CGScrollEventUnit::Pixel, *dy, *dx),
                };
                // Positive wheel1 scrolls up. Horizontal: our positive = right; CG's wheel2
                // positive = left, hence the sign flip (to be confirmed on hardware).
                let e = CGEvent::new_scroll_wheel_event2(Some(&self.source), units, 2, dy, -dx, 0);
                self.post(e)
            }
            InputEvent::Key {
                page, usage, state, ..
            } => {
                if *page != PAGE_KEYBOARD {
                    return Err(InputError::Unsupported(format!(
                        "HID page {page:#x} (media keys need NX system events)"
                    )));
                }
                let key = hid::keyboard_key(*usage)
                    .ok_or_else(|| InputError::Unsupported(format!("HID usage {usage:#x}")))?;
                if key.mac == 0xFFFF {
                    return Err(InputError::Unsupported(format!(
                        "{} has no Mac key",
                        key.name
                    )));
                }
                let down = *state != KeyState::Up;
                if let Some(flag) = Self::modifier_flag(key.mac) {
                    self.flags.set(flag, down);
                }
                let e = CGEvent::new_keyboard_event(Some(&self.source), key.mac, down);
                if let Some(e) = &e
                    && *state == KeyState::Repeat
                {
                    CGEvent::set_integer_value_field(
                        Some(e),
                        CGEventField::KeyboardEventAutorepeat,
                        1,
                    );
                }
                self.post(e)
            }
            InputEvent::Text { text } => {
                if text.len() > MAX_TEXT_BYTES {
                    return Err(InputError::Invalid("text too long".into()));
                }
                let units: Vec<u16> = text.encode_utf16().collect();
                // Up to 20 UTF-16 units per event is what CGEventKeyboardSetUnicodeString honours.
                for chunk in units.chunks(20) {
                    for down in [true, false] {
                        let e = CGEvent::new_keyboard_event(Some(&self.source), 0, down);
                        if let Some(e) = &e {
                            // SAFETY: `chunk` is a live slice of `chunk.len()` UTF-16 units.
                            unsafe {
                                CGEvent::keyboard_set_unicode_string(
                                    Some(e),
                                    chunk.len() as u64,
                                    chunk.as_ptr(),
                                )
                            };
                        }
                        self.post(e)?;
                    }
                }
                Ok(())
            }
            InputEvent::Touch { .. } | InputEvent::Pen { .. } => Err(InputError::Unsupported(
                "touch and pen are not injectable on macOS".into(),
            )),
            InputEvent::ReleaseAll => {
                self.flags = CGEventFlags::empty();
                Ok(())
            }
        }
    }
}
