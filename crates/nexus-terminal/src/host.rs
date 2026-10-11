//! PTY hosting.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use parking_lot::Mutex;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::TermError;

/// Output kept for re-attaching clients.
const SCROLLBACK_BYTES: usize = 1 << 20;

/// What the target allows.
#[derive(Debug, Clone)]
pub struct HostConfig {
    /// Programs a controller may ask for. Empty means only the default shell.
    pub allowed_shells: Vec<String>,
    /// The shell used when the controller does not ask for one (`None`: [`default_shell`]).
    pub default_shell: Option<String>,
    /// How long a detached persistent terminal is kept before it is closed.
    pub detached_ttl: Duration,
    /// Most terminals alive at once.
    pub max_sessions: usize,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            allowed_shells: Vec::new(),
            default_shell: None,
            detached_ttl: Duration::from_secs(24 * 3600),
            max_sessions: 16,
        }
    }
}

/// The login shell of this machine's user.
#[must_use]
pub fn default_shell() -> String {
    if cfg!(windows) {
        // PowerShell if present is what people expect; cmd.exe always exists.
        let ps = std::path::Path::new(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe");
        if ps.exists() {
            return ps.display().to_string();
        }
        return "cmd.exe".to_owned();
    }
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/sh".to_owned())
}

/// What a session sends to whoever is attached.
#[derive(Debug)]
pub(crate) enum Out {
    Data(Bytes),
    Exit(i32),
    /// Another controller took the terminal over.
    TakenOver,
}

#[derive(Default)]
struct State {
    scrollback: Vec<u8>,
    attached: Option<(u64, mpsc::UnboundedSender<Out>)>,
    exit: Option<i32>,
    detached_since: Option<Instant>,
    next_attach: u64,
}

/// One running shell.
pub(crate) struct Session {
    pub(crate) name: Option<String>,
    pub(crate) shell: String,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
    state: Mutex<State>,
}

/// An attachment to a [`Session`].
pub(crate) struct Attachment {
    pub(crate) id: u64,
    pub(crate) output: mpsc::UnboundedReceiver<Out>,
    pub(crate) reattached: bool,
}

impl Session {
    fn spawn(
        name: Option<String>,
        shell: &str,
        args: &[String],
        cwd: &str,
        size: PtySize,
    ) -> Result<Arc<Self>, TermError> {
        let pair = native_pty_system()
            .openpty(size)
            .map_err(|e| TermError::Spawn(e.to_string()))?;
        let mut cmd = CommandBuilder::new(shell);
        cmd.args(args);
        if !cwd.is_empty() {
            cmd.cwd(cwd);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| TermError::Spawn(e.to_string()))?;
        // The child holds the slave; ours must be closed or reads never see EOF.
        drop(pair.slave);
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| TermError::Spawn(e.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| TermError::Spawn(e.to_string()))?;
        let session = Arc::new(Self {
            name,
            shell: shell.to_owned(),
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            killer: Mutex::new(child.clone_killer()),
            state: Mutex::new(State::default()),
        });
        let s = Arc::clone(&session);
        std::thread::Builder::new()
            .name("pty-reader".into())
            .spawn(move || {
                let mut buf = vec![0u8; 16 * 1024];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => s.push_output(&buf[..n]),
                    }
                }
                let code = child
                    .wait()
                    .ok()
                    .and_then(|st| i32::try_from(st.exit_code()).ok())
                    .unwrap_or(-1);
                s.finish(code);
            })
            .map_err(|e| TermError::Spawn(e.to_string()))?;
        Ok(session)
    }

    fn push_output(&self, data: &[u8]) {
        let mut st = self.state.lock();
        st.scrollback.extend_from_slice(data);
        if st.scrollback.len() > SCROLLBACK_BYTES {
            let drop_n = st.scrollback.len() - SCROLLBACK_BYTES;
            st.scrollback.drain(..drop_n);
        }
        if let Some((_, tx)) = &st.attached {
            let _ = tx.send(Out::Data(Bytes::copy_from_slice(data)));
        }
    }

    fn finish(&self, code: i32) {
        let mut st = self.state.lock();
        st.exit = Some(code);
        if let Some((_, tx)) = &st.attached {
            let _ = tx.send(Out::Exit(code));
        }
    }

    pub(crate) fn attach(&self) -> Attachment {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut st = self.state.lock();
        if let Some((_, old)) = st.attached.take() {
            let _ = old.send(Out::TakenOver);
        }
        // Replay and subscribe under one lock so no output falls between the two.
        if !st.scrollback.is_empty() {
            let _ = tx.send(Out::Data(Bytes::copy_from_slice(&st.scrollback)));
        }
        if let Some(code) = st.exit {
            let _ = tx.send(Out::Exit(code));
        }
        st.next_attach += 1;
        let id = st.next_attach;
        st.attached = Some((id, tx));
        st.detached_since = None;
        Attachment {
            id,
            output: rx,
            reattached: false,
        }
    }

    pub(crate) fn detach(&self, id: u64) {
        let mut st = self.state.lock();
        if st.attached.as_ref().is_some_and(|(a, _)| *a == id) {
            st.attached = None;
            st.detached_since = Some(Instant::now());
        }
    }

    pub(crate) fn write(&self, data: &[u8]) -> std::io::Result<()> {
        let mut w = self.writer.lock();
        w.write_all(data)?;
        w.flush()
    }

    pub(crate) fn resize(&self, cols: u16, rows: u16) {
        let _ = self.master.lock().resize(PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    pub(crate) fn kill(&self) {
        let _ = self.killer.lock().kill();
    }

    fn exited(&self) -> bool {
        self.state.lock().exit.is_some()
    }

    fn detached_for(&self) -> Option<Duration> {
        self.state.lock().detached_since.map(|t| t.elapsed())
    }
}

/// Hosts terminals for one device.
pub struct TerminalHost {
    config: HostConfig,
    named: Mutex<HashMap<String, Arc<Session>>>,
    live: Mutex<Vec<Arc<Session>>>,
}

impl std::fmt::Debug for TerminalHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalHost")
            .field("sessions", &self.live.lock().len())
            .finish_non_exhaustive()
    }
}

impl TerminalHost {
    /// A host with `config`.
    #[must_use]
    pub fn new(config: HostConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            named: Mutex::new(HashMap::new()),
            live: Mutex::new(Vec::new()),
        })
    }

    /// Terminals currently alive (attached or detached, running or finished but not reaped).
    #[must_use]
    pub fn session_count(&self) -> usize {
        self.live.lock().len()
    }

    /// Names of persistent terminals.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.named.lock().keys().cloned().collect();
        v.sort();
        v
    }

    /// Closes finished terminals and detached ones past their time to live.
    pub fn reap(&self) {
        let ttl = self.config.detached_ttl;
        let mut live = self.live.lock();
        live.retain(|s| {
            let gone = s.exited() && s.detached_for().is_some()
                || s.detached_for().is_some_and(|d| d > ttl);
            if gone {
                debug!(name = ?s.name, "reaping a terminal");
                s.kill();
                if let Some(n) = &s.name {
                    self.named.lock().remove(n);
                }
            }
            !gone
        });
    }

    /// Reaps once a minute until the host is dropped.
    pub fn spawn_reaper(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            loop {
                tick.tick().await;
                let Some(host) = weak.upgrade() else { return };
                host.reap();
            }
        });
    }

    fn check_shell(&self, wanted: &str) -> Result<String, TermError> {
        if wanted.is_empty() {
            return Ok(self
                .config
                .default_shell
                .clone()
                .unwrap_or_else(default_shell));
        }
        if self.config.allowed_shells.iter().any(|s| s == wanted)
            || self.config.default_shell.as_deref() == Some(wanted)
            || (self.config.default_shell.is_none() && wanted == default_shell())
        {
            Ok(wanted.to_owned())
        } else {
            Err(TermError::ShellNotAllowed(wanted.to_owned()))
        }
    }

    /// Starts (or re-attaches to) a terminal.
    pub(crate) fn open(
        &self,
        req: &nexus_proto::v1::TermOpen,
    ) -> Result<(Arc<Session>, Attachment), TermError> {
        self.reap();
        let name = (!req.name.is_empty()).then(|| req.name.clone());
        if let Some(n) = &name
            && let Some(existing) = self.named.lock().get(n).cloned()
        {
            info!(name = %n, "re-attaching to a terminal");
            existing.resize(
                u16::try_from(req.cols).unwrap_or(80),
                u16::try_from(req.rows).unwrap_or(24),
            );
            let mut att = existing.attach();
            att.reattached = true;
            return Ok((existing, att));
        }
        if req.reattach_only {
            return Err(TermError::NoSuchSession(req.name.clone()));
        }
        if self.live.lock().len() >= self.config.max_sessions {
            return Err(TermError::TooMany);
        }
        let shell = self.check_shell(&req.shell)?;
        let size = PtySize {
            rows: u16::try_from(req.rows).unwrap_or(24).max(1),
            cols: u16::try_from(req.cols).unwrap_or(80).max(1),
            pixel_width: 0,
            pixel_height: 0,
        };
        let session = Session::spawn(name.clone(), &shell, &req.args, &req.cwd, size)?;
        info!(shell = %shell, name = ?name, "terminal started");
        if let Some(n) = name {
            self.named.lock().insert(n, Arc::clone(&session));
        }
        self.live.lock().push(Arc::clone(&session));
        let att = session.attach();
        Ok((session, att))
    }

    /// Called when an attachment ends: ephemeral terminals die with it.
    pub(crate) fn detached(&self, session: &Arc<Session>, id: u64) {
        session.detach(id);
        if session.name.is_none() {
            session.kill();
            self.live.lock().retain(|s| !Arc::ptr_eq(s, session));
        }
    }
}

impl Drop for TerminalHost {
    fn drop(&mut self) {
        for s in self.live.lock().iter() {
            s.kill();
        }
        if !self.live.lock().is_empty() {
            warn!("terminal host dropped with live shells; they were killed");
        }
    }
}
