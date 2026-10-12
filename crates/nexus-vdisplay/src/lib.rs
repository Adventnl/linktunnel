//! A screen for machines that have none.
//!
//! A headless Linux server has no monitor, no X server and no desktop, so there is nothing to
//! capture. [`VirtualDisplay`] starts a private, TCP-less `Xvfb` on a free display number and
//! (optionally) supervises a session command on it, such as a window manager or a terminal. The
//! existing X11 capture and input backends then work on it exactly as on a real screen
//! (`X11Backend::connect(Some(display.name()))`). Software encoding is used because such a
//! machine usually has no GPU.
//!
//! Non-Linux platforms always report [`VdError::Unsupported`]: macOS and Windows have their own
//! mechanisms, and the product only needs this on servers.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

/// What can go wrong starting a virtual display.
#[derive(Debug, thiserror::Error)]
pub enum VdError {
    /// This platform (or machine) cannot do it.
    #[error("virtual display unavailable: {0}")]
    Unsupported(String),
    /// The server started but never became ready.
    #[error("virtual display failed to start: {0}")]
    Start(String),
    /// A setting is out of range.
    #[error("invalid virtual display setting: {0}")]
    Invalid(String),
}

/// How to build the display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualDisplayConfig {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Colour depth (24 or 32).
    pub depth: u32,
    /// Dots per inch the server reports (scaling hint for desktop environments).
    pub dpi: u32,
    /// A command (program and arguments) to run on the display and keep running, e.g.
    /// `["openbox-session"]`. Empty = just the bare server.
    pub session: Vec<String>,
    /// The Xvfb binary; searched for on `PATH` when `None`.
    pub xvfb: Option<PathBuf>,
}

impl Default for VirtualDisplayConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            depth: 24,
            dpi: 96,
            session: Vec::new(),
            xvfb: None,
        }
    }
}

impl VirtualDisplayConfig {
    /// Parses `"1920x1080"`.
    pub fn with_size(mut self, spec: &str) -> Result<Self, VdError> {
        let (w, h) = spec
            .split_once(['x', 'X'])
            .ok_or_else(|| VdError::Invalid(format!("`{spec}` is not WIDTHxHEIGHT")))?;
        self.width = w
            .trim()
            .parse()
            .map_err(|_| VdError::Invalid(format!("width in `{spec}`")))?;
        self.height = h
            .trim()
            .parse()
            .map_err(|_| VdError::Invalid(format!("height in `{spec}`")))?;
        self.validate()?;
        Ok(self)
    }

    /// Rejects sizes no encoder or X server will accept.
    pub fn validate(&self) -> Result<(), VdError> {
        if !(320..=7680).contains(&self.width) || !(240..=4320).contains(&self.height) {
            return Err(VdError::Invalid(format!(
                "{}x{} is outside 320x240 .. 7680x4320",
                self.width, self.height
            )));
        }
        if !matches!(self.depth, 24 | 32) {
            return Err(VdError::Invalid(format!("depth {} (24 or 32)", self.depth)));
        }
        if !(48..=480).contains(&self.dpi) {
            return Err(VdError::Invalid(format!("dpi {} (48..=480)", self.dpi)));
        }
        Ok(())
    }
}

/// Finds an executable on `PATH`.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// True if a virtual display could be started here.
#[must_use]
pub fn available() -> bool {
    cfg!(target_os = "linux") && which("Xvfb").is_some()
}

/// A running virtual display. Dropping it stops the session command and the server.
#[derive(Debug)]
pub struct VirtualDisplay {
    server: Child,
    name: String,
    stop: Arc<AtomicBool>,
    session: Arc<Mutex<Option<Child>>>,
    supervisor: Option<JoinHandle<()>>,
    restarts: Arc<AtomicU32>,
    size: (u32, u32),
}

impl VirtualDisplay {
    /// Starts the server (and the session command) and waits until clients can connect.
    pub fn start(cfg: &VirtualDisplayConfig) -> Result<Self, VdError> {
        cfg.validate()?;
        if !cfg!(target_os = "linux") {
            return Err(VdError::Unsupported(
                "only Linux servers use a virtual display".into(),
            ));
        }
        let xvfb = cfg.xvfb.clone().or_else(|| which("Xvfb")).ok_or_else(|| {
            VdError::Unsupported("Xvfb is not installed (apt install xvfb)".into())
        })?;
        let (server, name) = start_server(&xvfb, cfg)?;
        info!(display = %name, size = %format!("{}x{}", cfg.width, cfg.height), "virtual display started");

        let stop = Arc::new(AtomicBool::new(false));
        let session = Arc::new(Mutex::new(None));
        let restarts = Arc::new(AtomicU32::new(0));
        let supervisor = (!cfg.session.is_empty()).then(|| {
            let (stop, session, restarts) = (
                Arc::clone(&stop),
                Arc::clone(&session),
                Arc::clone(&restarts),
            );
            let (argv, display) = (cfg.session.clone(), name.clone());
            std::thread::Builder::new()
                .name("nexus-vdisplay-session".into())
                .spawn(move || supervise(&argv, &display, &stop, &session, &restarts))
        });
        let supervisor = match supervisor {
            Some(Ok(h)) => Some(h),
            Some(Err(e)) => {
                return Err(VdError::Start(format!(
                    "cannot start the session supervisor: {e}"
                )));
            }
            None => None,
        };
        Ok(Self {
            server,
            name,
            stop,
            session,
            supervisor,
            restarts,
            size: (cfg.width, cfg.height),
        })
    }

    /// The `DISPLAY` value (`":123"`) clients connect to.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Screen size in pixels.
    #[must_use]
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Times the session command had to be restarted.
    #[must_use]
    pub fn session_restarts(&self) -> u32 {
        self.restarts.load(Ordering::Relaxed)
    }

    /// False once the X server has exited (the display is gone and must be recreated).
    pub fn is_alive(&mut self) -> bool {
        self.server.try_wait().ok().flatten().is_none()
    }
}

impl Drop for VirtualDisplay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Ok(mut s) = self.session.lock()
            && let Some(child) = s.as_mut()
        {
            terminate(child);
        }
        if let Some(h) = self.supervisor.take() {
            let _ = h.join();
        }
        terminate(&mut self.server);
        // A killed server cannot remove its own socket and lock file; do it so they do not pile up.
        let n = self.name.trim_start_matches(':');
        let _ = std::fs::remove_file(format!("/tmp/.X11-unix/X{n}"));
        let _ = std::fs::remove_file(format!("/tmp/.X{n}-lock"));
    }
}

/// Asks the process to exit (SIGTERM, so it can clean up) and kills it if it will not.
fn terminate(child: &mut Child) {
    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn start_server(xvfb: &PathBuf, cfg: &VirtualDisplayConfig) -> Result<(Child, String), VdError> {
    // Display numbers are probed rather than asked for (`-displayfd` needs an inherited file
    // descriptor, which safe Rust cannot pass). A taken number makes Xvfb exit at once, so the
    // loop simply tries the next one.
    // A process-wide counter keeps concurrent starts from racing for the same number.
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let base = 90 + (std::process::id() % 300);
    for _ in 0..60 {
        let n = base + NEXT.fetch_add(1, Ordering::SeqCst);
        if std::path::Path::new(&format!("/tmp/.X11-unix/X{n}")).exists()
            || std::path::Path::new(&format!("/tmp/.X{n}-lock")).exists()
        {
            continue;
        }
        let name = format!(":{n}");
        let mut child = Command::new(xvfb)
            .args([
                &name,
                "-screen",
                "0",
                &format!("{}x{}x{}", cfg.width, cfg.height, cfg.depth),
                "-dpi",
                &cfg.dpi.to_string(),
                "-nolisten",
                "tcp",
                "-noreset",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| VdError::Start(format!("cannot run {}: {e}", xvfb.display())))?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if child.try_wait().ok().flatten().is_some() {
                break; // the number was taken; try the next one
            }
            // The socket may belong to a different server that won the race for this number; the
            // lock file names the owner, and it must be us.
            let owner = std::fs::read_to_string(format!("/tmp/.X{n}-lock")).ok();
            if std::path::Path::new(&format!("/tmp/.X11-unix/X{n}")).exists()
                && owner.is_some_and(|o| o.trim().parse::<u32>() == Ok(child.id()))
            {
                return Ok((child, name));
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        if child.try_wait().ok().flatten().is_none() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(VdError::Start(
                "the X server did not open its socket in time".into(),
            ));
        }
    }
    Err(VdError::Start("no free display number".into()))
}

/// Runs the session command, restarting it with a growing pause whenever it exits.
fn supervise(
    argv: &[String],
    display: &str,
    stop: &AtomicBool,
    slot: &Mutex<Option<Child>>,
    restarts: &AtomicU32,
) {
    let mut backoff = Duration::from_millis(500);
    while !stop.load(Ordering::SeqCst) {
        let started = Instant::now();
        let spawned = Command::new(&argv[0])
            .args(&argv[1..])
            .env("DISPLAY", display)
            .env_remove("WAYLAND_DISPLAY")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match spawned {
            Ok(child) => {
                if let Ok(mut s) = slot.lock() {
                    *s = Some(child);
                }
            }
            Err(e) => warn!(command = %argv[0], error = %e, "cannot start the session command"),
        }
        // Wait for it to exit or for us to be told to stop.
        loop {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            let exited = slot
                .lock()
                .ok()
                .and_then(|mut s| s.as_mut().map(|c| c.try_wait().ok().flatten().is_some()))
                .unwrap_or(true);
            if exited {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if started.elapsed() > Duration::from_secs(60) {
            backoff = Duration::from_millis(500); // it ran for a while: a one-off crash
        }
        debug!(?backoff, "session command exited; restarting");
        restarts.fetch_add(1, Ordering::Relaxed);
        let until = Instant::now() + backoff;
        while Instant::now() < until {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn size_parsing_and_limits() {
        let c = VirtualDisplayConfig::default()
            .with_size("1280x720")
            .unwrap();
        assert_eq!((c.width, c.height), (1280, 720));
        assert!(VirtualDisplayConfig::default().with_size("1280").is_err());
        assert!(VirtualDisplayConfig::default().with_size("axb").is_err());
        assert!(VirtualDisplayConfig::default().with_size("10x10").is_err());
        assert!(
            VirtualDisplayConfig::default()
                .with_size("99999x99999")
                .is_err()
        );
        let c = VirtualDisplayConfig {
            depth: 8,
            ..VirtualDisplayConfig::default()
        };
        assert!(c.validate().is_err());
    }
}
