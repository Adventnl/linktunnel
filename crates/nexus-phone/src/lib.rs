//! Local Android control using separately installed adb and scrcpy (3.3 or newer).
//!
//! Android grants access through its own USB authorisation or wireless pairing. This
//! transport is independent of linktunnel agent pairing, sessions and lockdown. No call
//! capture or microphone injection is claimed. Media stays in the native viewer.
#![forbid(unsafe_code)]

mod process;

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, Notify};

/// Failures are returned to callers; tool output never goes to application logs.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Invalid user input.
    #[error("{0}")]
    Invalid(&'static str),
    /// Tool execution or transport failure.
    #[error("{0}")]
    Tool(String),
    /// A session is already running.
    #[error("Stop the current phone session before opening another one.")]
    Busy,
}

/// An adb-visible transport, including unavailable and unauthorised phones.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    /// Exact adb transport identifier (never an implicit first device).
    pub serial: String,
    /// adb state, e.g. `device`, `offline`, `unauthorized`.
    pub state: String,
    /// Model reported by adb, or the serial if unavailable.
    pub model: String,
}

/// Options for a single native phone window.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Options {
    /// The authorised adb transport to control.
    pub serial: String,
    /// Keep the physical display off while mirroring.
    pub screen_off: bool,
    /// Forward ordinary media audio to the computer; does not forward cellular calls.
    pub media_audio: bool,
    /// Maximum encoded dimension (720–2560).
    pub max_size: u16,
    /// Frame rate limit (15–120).
    pub max_fps: u16,
}

impl Options {
    fn args(&self) -> Result<Vec<String>, Error> {
        validate_serial(&self.serial)?;
        if !(720..=2560).contains(&self.max_size) || !(15..=120).contains(&self.max_fps) {
            return Err(Error::Invalid(
                "Unsupported phone resolution or frame rate.",
            ));
        }
        let mut args = vec![
            format!("--serial={}", self.serial),
            "--window-title=linktunnel Phone".into(),
            format!("--max-size={}", self.max_size),
            format!("--max-fps={}", self.max_fps),
            "--video-bit-rate=8M".into(),
            "--video-codec=h264".into(),
            "--verbosity=info".into(),
            "--print-fps".into(),
        ];
        if self.screen_off {
            args.push("--turn-screen-off".into());
        }
        if !self.media_audio {
            args.push("--no-audio".into());
        }
        Ok(args)
    }
}

/// Current managed window and its last exit diagnostic.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    /// Present only while the scrcpy process is running and has received a frame.
    pub serial: Option<String>,
    /// Error from the most recent unexpectedly closed session.
    pub error: Option<String>,
}

/// Prerequisites and current phone session; works without a linktunnel agent.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// adb version, or diagnostic when absent.
    pub adb: Result<String, String>,
    /// scrcpy version, or diagnostic when absent.
    pub scrcpy: Result<String, String>,
    /// OS running this desktop app.
    pub platform: &'static str,
    /// Actual session state.
    pub session: Session,
}

#[derive(Default)]
struct Managed {
    child: Option<Child>,
    logs: Option<process::Logs>,
    session: Session,
}

/// Owns one native phone window. Drop kills it; callers should also call `stop`
/// before exiting to reap the process and let Android restore display state.
pub struct Phone {
    adb: PathBuf,
    scrcpy: PathBuf,
    managed: Mutex<Managed>,
    cancel: Notify,
}

impl Default for Phone {
    fn default() -> Self {
        let scrcpy: PathBuf = std::env::var_os("NEXUS_SCRCPY")
            .unwrap_or_else(|| "scrcpy".into())
            .into();
        // Portable Windows scrcpy distributions ship adb next to scrcpy.exe.
        let adjacent = scrcpy.with_file_name(if cfg!(windows) { "adb.exe" } else { "adb" });
        let adb = std::env::var_os("NEXUS_ADB").map_or_else(
            || {
                if scrcpy.is_absolute() && adjacent.is_file() {
                    adjacent
                } else {
                    "adb".into()
                }
            },
            PathBuf::from,
        );
        Self {
            adb,
            scrcpy,
            managed: Mutex::new(Managed::default()),
            cancel: Notify::new(),
        }
    }
}

impl Phone {
    fn adb(&self) -> Command {
        process::command(&self.adb)
    }

    /// Check installed prerequisites and reap a window that the user closed.
    pub async fn status(&self) -> Status {
        let mut adb_cmd = self.adb();
        let mut scrcpy_cmd = process::command(&self.scrcpy);
        let (adb, scrcpy) = tokio::join!(
            process::output(adb_cmd.arg("version"), Duration::from_secs(5)),
            process::output(scrcpy_cmd.arg("--version"), Duration::from_secs(5)),
        );
        Status {
            adb: adb
                .map(|v| v.lines().next().unwrap_or_default().to_owned())
                .map_err(|e| e.to_string()),
            scrcpy: scrcpy
                .and_then(|v| check_version(&v))
                .map_err(|e| e.to_string()),
            platform: std::env::consts::OS,
            session: self.session().await,
        }
    }

    /// Enumerate transports. Unauthorised devices remain visible for setup help.
    pub async fn devices(&self) -> Result<Vec<Device>, Error> {
        let out =
            process::output(self.adb().args(["devices", "-l"]), Duration::from_secs(10)).await?;
        parse_devices(&out)
    }

    /// Pair with Android 11+ wireless debugging. Code travels on stdin, never argv.
    pub async fn pair(&self, address: &str, code: &str) -> Result<(), Error> {
        validate_address(address)?;
        if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Error::Invalid("Enter the six-digit wireless pairing code."));
        }
        let mut cmd = self.adb();
        cmd.args(["pair", address]).stdin(Stdio::piped());
        let mut child = process::spawn(&mut cmd)?;
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| Error::Tool("Pairing input unavailable.".into()))?;
        input
            .write_all(code.as_bytes())
            .await
            .map_err(|e| Error::Tool(e.to_string()))?;
        input
            .write_all(b"\n")
            .await
            .map_err(|e| Error::Tool(e.to_string()))?;
        drop(input);
        // Never return pairing output: some adb versions echo the pairing code.
        let out = process::collect(child, Duration::from_secs(45))
            .await
            .map_err(|_| {
                Error::Tool(
                    "Wireless pairing failed. Check the pairing address and request a fresh code."
                        .into(),
                )
            })?;
        if !out.contains("Successfully paired") {
            return Err(Error::Tool(
                "Wireless pairing failed. Check the pairing address and request a fresh code."
                    .into(),
            ));
        }
        Ok(())
    }

    /// Connect using the connection port (different from the pairing port).
    pub async fn connect(&self, address: &str) -> Result<(), Error> {
        validate_address(address)?;
        let out = process::output(
            self.adb().args(["connect", address]),
            Duration::from_secs(15),
        )
        .await?;
        if !(out.starts_with("connected to ") || out.starts_with("already connected to ")) {
            return Err(Error::Tool(out));
        }
        Ok(())
    }

    /// Disconnect one explicit network transport without killing the shared adb server.
    pub async fn disconnect(&self, address: &str) -> Result<(), Error> {
        validate_address(address)?;
        if self.session().await.serial.as_deref() == Some(address) {
            self.stop().await?;
        }
        process::output(
            self.adb().args(["disconnect", address]),
            Duration::from_secs(10),
        )
        .await?;
        Ok(())
    }

    /// Open the selected device, waiting for a decoded frame before reporting success.
    pub async fn start(&self, options: Options) -> Result<(), Error> {
        // Register before acquiring the session lock so Stop/desktop exit can also
        // cancel version checks, adb enumeration and first-frame waits.
        let cancelled = self.cancel.notified();
        tokio::pin!(cancelled);
        cancelled.as_mut().enable();
        tokio::select! {
            biased;
            _ = cancelled => Err(Error::Tool("Phone connection cancelled.".into())),
            result = self.start_inner(options) => result,
        }
    }

    async fn start_inner(&self, options: Options) -> Result<(), Error> {
        let args = options.args()?;
        let mut managed = self.managed.lock().await;
        managed.refresh()?;
        if managed.child.is_some() {
            return Err(Error::Busy);
        }
        let version = process::output(
            process::command(&self.scrcpy).arg("--version"),
            Duration::from_secs(5),
        )
        .await?;
        check_version(&version)?;
        let devices = self.devices().await?;
        if !devices
            .iter()
            .any(|d| d.serial == options.serial && d.state == "device")
        {
            return Err(Error::Invalid(
                "Select an online, authorised phone. Accept the debugging prompt on the phone first.",
            ));
        }
        let mut cmd = process::command(&self.scrcpy);
        cmd.args(args).env("ADB", &self.adb);
        let (child, logs) = process::viewer(&mut cmd, Duration::from_secs(30)).await?;
        managed.child = Some(child);
        managed.logs = Some(logs);
        managed.session = Session {
            serial: Some(options.serial),
            error: None,
        };
        Ok(())
    }

    /// Report a user-closed window or disconnected phone accurately.
    pub async fn session(&self) -> Session {
        let mut managed = self.managed.lock().await;
        if let Err(e) = managed.refresh() {
            managed.session.error = Some(e.to_string());
        }
        managed.session.clone()
    }

    /// End the owned window. Does not revoke Android's saved debugging authorisation.
    pub async fn stop(&self) -> Result<(), Error> {
        self.cancel.notify_waiters();
        let mut managed = self.managed.lock().await;
        managed.logs = None;
        managed.session = Session::default();
        if let Some(mut child) = managed.child.take() {
            child.kill().await.map_err(|e| Error::Tool(e.to_string()))?;
        }
        Ok(())
    }
}

impl Managed {
    fn refresh(&mut self) -> Result<(), Error> {
        let Some(child) = self.child.as_mut() else {
            return Ok(());
        };
        if let Some(exit) = child.try_wait().map_err(|e| Error::Tool(e.to_string()))? {
            self.session.serial = None;
            self.session.error = if exit.success() {
                None
            } else {
                Some(
                    self.logs
                        .as_ref()
                        .map(process::Logs::tail)
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| format!("Phone viewer exited: {exit}")),
                )
            };
            self.child = None;
            self.logs = None;
        }
        Ok(())
    }
}

fn validate_serial(serial: &str) -> Result<(), Error> {
    if serial.is_empty()
        || serial.len() > 256
        || serial.starts_with('-')
        || !serial
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".:_-[]%".contains(&b))
    {
        return Err(Error::Invalid("Invalid adb device identifier."));
    }
    Ok(())
}

fn validate_address(address: &str) -> Result<(), Error> {
    // Numeric IPs only: prevents flags, shell metacharacters and ambiguous host/port parsing.
    let parsed = address.parse::<std::net::SocketAddr>().map_err(|_| {
        Error::Invalid("Use an IP address and port, for example 192.168.1.20:37123 or [::1]:37123.")
    })?;
    if parsed.port() == 0 || parsed.ip().is_unspecified() || parsed.ip().is_multicast() {
        return Err(Error::Invalid(
            "Use the phone's reachable IP address and a nonzero port.",
        ));
    }
    Ok(())
}

fn check_version(out: &str) -> Result<String, Error> {
    let line = out
        .lines()
        .find(|l| l.starts_with("scrcpy "))
        .ok_or_else(|| {
            Error::Tool("Could not identify scrcpy. Install scrcpy 3.3 or newer.".into())
        })?;
    let version = line.split_whitespace().nth(1).unwrap_or_default();
    let mut parts = version.split('.');
    let major = parts
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    let minor = parts
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    if major < 3 || (major == 3 && minor < 3) {
        return Err(Error::Tool("Install scrcpy 3.3 or newer.".into()));
    }
    Ok(line.to_owned())
}

fn parse_devices(out: &str) -> Result<Vec<Device>, Error> {
    if out.len() > 1 << 20 {
        return Err(Error::Invalid("adb device list is too large."));
    }
    let mut devices = Vec::new();
    for line in out.lines() {
        let mut parts = line.split_whitespace();
        let (Some(serial), Some(state)) = (parts.next(), parts.next()) else {
            continue;
        };
        if line.starts_with("List of devices") || line.starts_with('*') {
            continue;
        }
        if validate_serial(serial).is_err() {
            continue;
        }
        let model = parts
            .find_map(|p| p.strip_prefix("model:"))
            .unwrap_or(serial)
            .replace('_', " ");
        devices.push(Device {
            serial: serial.into(),
            state: state.into(),
            model,
        });
    }
    Ok(devices)
}

#[cfg(test)]
mod tests;
