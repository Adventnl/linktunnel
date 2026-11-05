//! Local Android control using bundled adb and scrcpy (3.3 or newer).
//!
//! Android grants access through its own USB authorisation or wireless pairing. This
//! transport is independent of linktunnel agent pairing, sessions and lockdown. No call
//! capture or microphone injection is claimed. Media stays in the native viewer.
#![deny(unsafe_code)]

pub mod calls;
mod process;
#[cfg(windows)]
#[allow(unsafe_code)]
mod window;

#[cfg(windows)]
pub use window::Bounds as WindowBounds;

use std::path::{Path, PathBuf};
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
    /// Startup is in progress; status queries never wait on that operation.
    pub starting: bool,
    /// Input permission warning, independent of whether video is working.
    pub warning: Option<String>,
    /// Initial decoded picture dimensions, when available from the native viewer.
    pub width: u32,
    /// Initial decoded picture height.
    pub height: u32,
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
    #[cfg(windows)]
    window: Option<window::Window>,
}

/// Owns one native phone window. Drop kills it; callers should also call `stop`
/// before exiting to reap the process and let Android restore display state.
pub struct Phone {
    adb: PathBuf,
    scrcpy: PathBuf,
    managed: Mutex<Managed>,
    cancel: Notify,
}

/// Directory name shared with the build-time phone-tools manifest.
fn tool_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", _) => "x86_64-apple-darwin",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        ("windows", "x86") => "i686-pc-windows-msvc",
        ("windows", _) => "x86_64-pc-windows-msvc",
        _ => "x86_64-unknown-linux-gnu",
    }
}

fn discover_bundle(executable: &Path) -> Option<PathBuf> {
    let parent = executable.parent()?;
    // CLI sidecar beside a Windows app, or inside a macOS .app's Contents/MacOS.
    let mut roots = vec![parent.join("phone-tools")];
    if let Some(contents) = parent.parent() {
        roots.push(contents.join("Resources/phone-tools"));
        // Tauri's Linux deb/AppImage resources sit under lib/<product>.
        roots.push(contents.join("lib/linktunnel/phone-tools"));
    }
    #[cfg(debug_assertions)]
    roots.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../apps/nexus-app/src-tauri/resources/phone-tools"),
    );
    roots
        .into_iter()
        .find(|root| root.join(tool_target()).is_dir())
}

impl Default for Phone {
    fn default() -> Self {
        match std::env::current_exe()
            .ok()
            .and_then(|exe| discover_bundle(&exe))
        {
            Some(root) => Self::bundled(root),
            None => Self::with_tools(None),
        }
    }
}

impl Phone {
    /// Use the app's resource directory, independent of Finder's PATH. Explicit developer
    /// NEXUS_ADB/NEXUS_SCRCPY overrides remain available for troubleshooting.
    #[must_use]
    pub fn bundled(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        #[cfg(debug_assertions)]
        let development = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../apps/nexus-app/src-tauri/resources/phone-tools");
        #[cfg(debug_assertions)]
        let root = if root.join(tool_target()).is_dir() {
            root
        } else {
            &development
        };
        Self::with_tools(Some(&root.join(tool_target())))
    }

    fn with_tools(bundle: Option<&Path>) -> Self {
        let executable = |name: &str| {
            let name = if cfg!(windows) {
                format!("{name}.exe")
            } else {
                name.to_owned()
            };
            bundle.map_or_else(|| PathBuf::from(&name), |dir| dir.join(&name))
        };
        let scrcpy = std::env::var_os("NEXUS_SCRCPY")
            .map(PathBuf::from)
            .unwrap_or_else(|| executable("scrcpy"));
        let adjacent = scrcpy.with_file_name(if cfg!(windows) { "adb.exe" } else { "adb" });
        let adb = std::env::var_os("NEXUS_ADB")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                if scrcpy.is_absolute() && adjacent.is_file() {
                    adjacent
                } else {
                    executable("adb")
                }
            });
        // Tauri canonicalizes resource_dir to a verbatim Windows path. The
        // bundled C tools' stat() cannot read that syntax, even for existing files.
        #[cfg(windows)]
        let (adb, scrcpy) = (
            dunce::simplified(&adb).to_owned(),
            dunce::simplified(&scrcpy).to_owned(),
        );
        Self {
            adb,
            scrcpy,
            managed: Mutex::new(Managed::default()),
            cancel: Notify::new(),
        }
    }

    fn scrcpy(&self) -> Command {
        let mut command = process::command(&self.scrcpy);
        command.env("ADB", &self.adb);
        let server = self.scrcpy.with_file_name("scrcpy-server");
        if self.scrcpy.is_absolute() && server.is_file() {
            command.env("SCRCPY_SERVER_PATH", server);
        }
        command
    }

    fn adb(&self) -> Command {
        process::command(&self.adb)
    }

    /// Check installed prerequisites and reap a window that the user closed.
    pub async fn status(&self) -> Status {
        let mut adb_cmd = self.adb();
        let mut scrcpy_cmd = self.scrcpy();
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
            result = tokio::time::timeout(Duration::from_secs(40), self.start_inner(options)) => {
                result.unwrap_or_else(|_| Err(Error::Tool("Phone connection timed out. Check Wireless debugging, unlock the phone, and try Connect again.".into())))
            },
        }
    }

    async fn start_inner(&self, options: Options) -> Result<(), Error> {
        let args = options.args()?;
        let mut managed = self.managed.lock().await;
        managed.refresh()?;
        if managed.child.is_some() {
            return Err(Error::Busy);
        }
        let version =
            process::output(self.scrcpy().arg("--version"), Duration::from_secs(5)).await?;
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
        let mut cmd = self.scrcpy();
        cmd.args(args);
        let (child, logs) = process::viewer(&mut cmd, Duration::from_secs(30)).await?;
        managed.child = Some(child);
        managed.logs = Some(logs);
        #[cfg(windows)]
        {
            managed.window = managed
                .child
                .as_ref()
                .and_then(Child::id)
                .and_then(window::visible);
        }
        managed.session = Session {
            serial: Some(options.serial),
            error: None,
            ..Session::default()
        };
        #[cfg(windows)]
        if let Some((width, height)) = managed
            .window
            .as_ref()
            .map(|window| (window.width, window.height))
        {
            managed.session.width = width;
            managed.session.height = height;
        }
        Ok(())
    }

    /// Report a user-closed window or disconnected phone accurately.
    pub async fn session(&self) -> Session {
        let Ok(mut managed) = self.managed.try_lock() else {
            return Session {
                starting: true,
                ..Session::default()
            };
        };
        if let Err(e) = managed.refresh() {
            managed.session.error = Some(e.to_string());
        }
        managed.session.clone()
    }

    /// Place only this Phone instance's owned native viewer in the dashboard.
    #[cfg(windows)]
    pub async fn layout(&self, parent: isize, bounds: WindowBounds) -> Result<(), Error> {
        // A layout update must never queue behind viewer startup or block cancellation.
        if let Ok(managed) = self.managed.try_lock()
            && let Some(window) = &managed.window
        {
            window.layout(parent, bounds).map_err(Error::Tool)?;
        }
        Ok(())
    }

    /// End the owned window. Does not revoke Android's saved debugging authorisation.
    pub async fn stop(&self) -> Result<(), Error> {
        self.cancel.notify_waiters();
        let mut managed = self.managed.lock().await;
        managed.logs = None;
        managed.session = Session::default();
        #[cfg(windows)]
        {
            managed.window = None;
        }
        if let Some(mut child) = managed.child.take() {
            child.kill().await.map_err(|e| Error::Tool(e.to_string()))?;
        }
        Ok(())
    }
}

impl Managed {
    fn refresh(&mut self) -> Result<(), Error> {
        if self
            .logs
            .as_ref()
            .is_some_and(|logs| logs.tail().contains("INJECT_EVENTS"))
        {
            self.session.warning = Some("input_permission".into());
        }
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
            Error::Tool(
                "Could not identify the phone viewer. Reinstall the complete linktunnel app."
                    .into(),
            )
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
        return Err(Error::Tool(
            "The phone viewer is outdated. Reinstall the complete linktunnel app.".into(),
        ));
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
