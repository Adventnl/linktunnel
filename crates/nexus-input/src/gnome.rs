//! Input for the existing GNOME Wayland session through Mutter's remote desktop API.
use crate::{Injector, InputError, InputEvent, KeyState, MonitorRect, ScrollUnit};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

fn failed(e: impl std::fmt::Display) -> InputError {
    InputError::Failed(e.to_string())
}
/// Owns one compositor-managed input session; disconnect releases its virtual devices.
pub struct GnomeInjector {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
}
impl GnomeInjector {
    /// Opens an input session in the logged-in GNOME desktop.
    pub fn connect() -> Result<Self, InputError> {
        let mut child = Command::new("/usr/bin/python3")
            .args(["-u", "-c", include_str!("gnome.py")])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(failed)?;
        let input = child.stdin.take();
        let output = child
            .stdout
            .take()
            .ok_or_else(|| failed("no input bridge output"))?;
        let mut injector = Self {
            child,
            input,
            output: BufReader::new(output),
        };
        let mut line = String::new();
        injector.output.read_line(&mut line).map_err(failed)?;
        if line.trim() != "ready" {
            return Err(InputError::Unavailable(
                "GNOME input bridge failed to start; see agent log".into(),
            ));
        }
        Ok(injector)
    }
    fn send(&mut self, event: &Value) -> Result<(), InputError> {
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| failed("input bridge closed"))?;
        writeln!(input, "{event}").map_err(failed)?;
        input.flush().map_err(failed)?;
        let mut response = String::new();
        self.output.read_line(&mut response).map_err(failed)?;
        if response.trim() != "ok" {
            return Err(failed(format!("GNOME input: {}", response.trim())));
        }
        Ok(())
    }
}
impl Injector for GnomeInjector {
    fn name(&self) -> &'static str {
        "gnome-wayland"
    }
    fn set_monitors(&mut self, _: &[MonitorRect]) {}
    fn inject(&mut self, event: &InputEvent) -> Result<(), InputError> {
        let command = match event {
            InputEvent::MouseMoveAbs { monitor, x, y } => json!(["abs", monitor, x, y]),
            InputEvent::MouseMoveRel { dx, dy } => json!(["rel", dx, dy]),
            InputEvent::MouseButton { button, pressed } => {
                let code = match button.0 {
                    0 => 272,
                    1 => 273,
                    2 => 274,
                    3 => 275,
                    4 => 276,
                    _ => return Err(InputError::Unsupported("unknown pointer button".into())),
                };
                json!(["button", code, pressed])
            }
            InputEvent::Key {
                page, usage, state, ..
            } => {
                if *state == KeyState::Repeat {
                    return Ok(());
                } // Mutter handles key repeat.
                let code = crate::hid::evdev_code(*page, *usage).ok_or_else(|| {
                    InputError::Invalid(format!("unknown HID key {page}:{usage}"))
                })?;
                json!(["key", code, *state == KeyState::Down])
            }
            InputEvent::Text { text } => json!(["text", text]),
            InputEvent::Scroll { dx, dy, unit, .. } => {
                let scale = if *unit == ScrollUnit::Detent {
                    15.0 / 120.0
                } else {
                    1.0
                };
                json!(["scroll", f64::from(*dx) * scale, -f64::from(*dy) * scale])
            }
            InputEvent::ReleaseAll => json!(["release"]),
            InputEvent::Touch { .. } | InputEvent::Pen { .. } => {
                return Err(InputError::Unsupported(
                    "GNOME touch and pen input not implemented".into(),
                ));
            }
        };
        self.send(&command)
    }
    fn release_native_state(&mut self) -> Result<(), InputError> {
        self.send(&json!(["release"]))
    }
}
impl Drop for GnomeInjector {
    fn drop(&mut self) {
        self.input.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
