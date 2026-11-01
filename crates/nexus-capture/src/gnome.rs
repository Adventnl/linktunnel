//! GNOME Wayland capture through Mutter and PipeWire, with an embedded PyGObject bridge.
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

use bytes::Bytes;
use nexus_codec::{Frame, FrameFormat};
use serde_json::Value;

use crate::{CaptureBackend, CaptureError, Capturer, CursorSource, MonitorInfo};

const BRIDGE: &str = include_str!("gnome.py");
fn failed(e: impl std::fmt::Display) -> CaptureError {
    CaptureError::Failed(e.to_string())
}
fn command() -> Command {
    let mut cmd = Command::new("/usr/bin/python3");
    cmd.args(["-u", "-c", BRIDGE]);
    cmd
}
#[allow(clippy::cast_possible_truncation)] // Monitor scales and refresh rates are bounded display metadata.
fn monitor(v: &Value) -> Result<MonitorInfo, CaptureError> {
    let u = |key: &str| {
        v[key]
            .as_u64()
            .ok_or_else(|| failed(format!("missing {key}")))
    };
    Ok(MonitorInfo {
        index: u8::try_from(u("index")?).map_err(failed)?,
        name: v["name"]
            .as_str()
            .ok_or_else(|| failed("missing connector"))?
            .into(),
        x: i32::try_from(v["x"].as_i64().ok_or_else(|| failed("missing x"))?).map_err(failed)?,
        y: i32::try_from(v["y"].as_i64().ok_or_else(|| failed("missing y"))?).map_err(failed)?,
        width: u32::try_from(u("width")?).map_err(failed)?,
        height: u32::try_from(u("height")?).map_err(failed)?,
        scale: v["scale"].as_f64().unwrap_or(1.0) as f32,
        refresh_hz: v["refresh_hz"].as_f64().unwrap_or(0.0) as f32,
        primary: v["primary"].as_bool().unwrap_or(false),
    })
}
/// Captures the existing GNOME Wayland desktop, without creating a virtual display.
#[derive(Debug)]
pub struct GnomeBackend;
impl GnomeBackend {
    /// Probes the current GNOME session and its monitor layout.
    pub fn connect() -> Result<Self, CaptureError> {
        let backend = Self;
        if backend.monitors()?.is_empty() {
            return Err(CaptureError::NoDisplay(
                "GNOME has no active monitors".into(),
            ));
        }
        Ok(backend)
    }
}
impl CaptureBackend for GnomeBackend {
    fn name(&self) -> &'static str {
        "gnome-pipewire"
    }
    fn monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError> {
        let result = command().arg("monitors").output().map_err(failed)?;
        if !result.status.success() {
            return Err(CaptureError::NoDisplay(
                String::from_utf8_lossy(&result.stderr).into_owned(),
            ));
        }
        let values: Vec<Value> = serde_json::from_slice(&result.stdout).map_err(failed)?;
        values.iter().map(monitor).collect()
    }
    fn open(&self, index: u8) -> Result<Box<dyn Capturer>, CaptureError> {
        let selected = self
            .monitors()?
            .into_iter()
            .find(|m| m.index == index)
            .ok_or(CaptureError::NoSuchMonitor(index))?;
        let child = command()
            .arg(index.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(failed)?;
        let mut capture = GnomeCapturer {
            child,
            input: None,
            output: None,
            monitor: selected,
        };
        capture.input = capture.child.stdin.take();
        capture.output = capture.child.stdout.take().map(BufReader::new);
        let mut line = String::new();
        capture
            .output
            .as_mut()
            .ok_or_else(|| failed("no capture output"))?
            .read_line(&mut line)
            .map_err(failed)?;
        capture.monitor = monitor(&serde_json::from_str::<Value>(&line).map_err(failed)?)?;
        Ok(Box::new(capture))
    }
    // Cursor is embedded by Mutter into the video frames.
    fn cursor_source(&self) -> Result<Option<Box<dyn CursorSource>>, CaptureError> {
        Ok(None)
    }
}
struct GnomeCapturer {
    child: Child,
    input: Option<ChildStdin>,
    output: Option<BufReader<ChildStdout>>,
    monitor: MonitorInfo,
}
impl Capturer for GnomeCapturer {
    fn name(&self) -> &'static str {
        "gnome-pipewire"
    }
    fn monitor(&self) -> &MonitorInfo {
        &self.monitor
    }
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>, CaptureError> {
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| failed("capture closed"))?;
        writeln!(input, "{}", timeout.as_millis().min(2000)).map_err(failed)?;
        input.flush().map_err(failed)?;
        let output = self
            .output
            .as_mut()
            .ok_or_else(|| failed("capture closed"))?;
        let mut header = [0; 16];
        output.read_exact(&mut header).map_err(failed)?;
        let value =
            |i| u32::from_le_bytes([header[i], header[i + 1], header[i + 2], header[i + 3]]);
        let (width, height, stride, size) = (value(0), value(4), value(8), value(12));
        if size == 0 {
            return Ok(None);
        }
        if width == 0
            || height == 0
            || width > 16384
            || height > 16384
            || stride < width * 4
            || u64::from(stride) * u64::from(height) != u64::from(size)
            || size > 256 * 1024 * 1024
        {
            return Err(failed("invalid PipeWire frame dimensions"));
        }
        let mut data = vec![0; size as usize];
        output.read_exact(&mut data).map_err(failed)?;
        Ok(Some(Frame {
            width,
            height,
            stride: stride as usize,
            format: FrameFormat::Bgra8,
            data: Bytes::from(data),
            captured_us: nexus_core::clock::now_us(),
            dirty: Vec::new(),
        }))
    }
}
impl Drop for GnomeCapturer {
    fn drop(&mut self) {
        self.input.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
