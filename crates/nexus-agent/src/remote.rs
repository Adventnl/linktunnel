//! The target-side session handler: serves remote-desktop sessions (and diagnostics).

use std::sync::Arc;

use async_trait::async_trait;
use nexus_capture::CaptureBackend;
use nexus_capture::headless::HeadlessBackend;
use nexus_clipboard::ClipboardSetup;
use nexus_files::{FileRoots, FileServer};
use nexus_media::target::{self, InjectorFactory, TargetConfig, TargetStats};
use nexus_power::PowerBackend;
use nexus_proto::features;
use nexus_proto::v1::Capabilities;
use tracing::{info, warn};

use crate::sessions::{SessionContext, SessionHandler};

/// Serves remote-desktop sessions from this machine's screen and input devices.
///
/// With no capture backend (a headless box without a virtual display) it still answers
/// diagnostics, but advertises no video, so controllers know to use a terminal instead.
pub struct RemoteHandler {
    capture: Option<Arc<dyn CaptureBackend>>,
    injector: Option<InjectorFactory>,
    max_fps: u32,
    files: Option<Arc<FileServer>>,
    clipboard: Option<ClipboardSetup>,
    power: Option<Arc<dyn PowerBackend>>,
    wake_relay: bool,
    terminal: Option<Arc<nexus_terminal::TerminalHost>>,
    portfwd: Option<Arc<nexus_portfwd::ForwardHost>>,
}

impl std::fmt::Debug for RemoteHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteHandler")
            .field("capture", &self.capture.as_ref().map(|c| c.name()))
            .field("input", &self.injector.is_some())
            .field("max_fps", &self.max_fps)
            .field("files", &self.files.is_some())
            .field("clipboard", &self.clipboard.is_some())
            .field("power", &self.power.is_some())
            .finish()
    }
}

impl RemoteHandler {
    /// Builds a handler from explicit parts (tests, embedding).
    #[must_use]
    pub fn new(
        capture: Option<Arc<dyn CaptureBackend>>,
        injector: Option<InjectorFactory>,
        max_fps: u32,
    ) -> Self {
        Self {
            capture,
            injector,
            max_fps: max_fps.clamp(1, 240),
            files: None,
            clipboard: None,
            power: None,
            wake_relay: false,
            terminal: None,
            portfwd: None,
        }
    }

    /// Also serves the file panel from these shared folders to peers holding the `files` grant.
    /// (With no folders nothing is served.)
    #[must_use]
    pub fn with_files(mut self, roots: FileRoots, max_upload_bytes: u64) -> Self {
        self.files =
            (!roots.is_empty()).then(|| Arc::new(FileServer::new(roots, max_upload_bytes)));
        self
    }

    /// Lets controllers holding the `power` grant lock / sleep / restart this machine.
    #[must_use]
    pub fn with_power(mut self, backend: Arc<dyn PowerBackend>) -> Self {
        self.power = Some(backend);
        self
    }

    /// Lets controllers holding the `terminal` grant open shells on this machine.
    #[must_use]
    pub fn with_terminal(mut self, host: Arc<nexus_terminal::TerminalHost>) -> Self {
        self.terminal = Some(host);
        self
    }

    /// Lets controllers holding the `terminal` grant forward TCP connections through this machine.
    #[must_use]
    pub fn with_port_forwarding(mut self, host: Arc<nexus_portfwd::ForwardHost>) -> Self {
        self.portfwd = Some(host);
        self
    }

    /// Lets controllers holding the `wake` grant use this machine to wake others on its LAN.
    #[must_use]
    pub fn with_wake_relay(mut self, on: bool) -> Self {
        self.wake_relay = on;
        self
    }

    /// Does this handler offer anything besides video (so a screenless machine is still worth a
    /// full session rather than just diagnostics)?
    fn serves_without_a_screen(&self) -> bool {
        self.files.is_some()
            || self.power.is_some()
            || self.wake_relay
            || self.clipboard.is_some()
            || self.terminal.is_some()
            || self.portfwd.is_some()
    }

    /// Syncs this machine's clipboard with controllers holding the `clipboard` grant.
    #[must_use]
    pub fn with_clipboard(mut self, setup: ClipboardSetup) -> Self {
        self.clipboard = Some(setup);
        self
    }

    /// Uses this machine's real screen and input if they are reachable, and says so in the log
    /// when they are not.
    #[must_use]
    pub fn detect(max_fps: u32) -> Self {
        let capture = match nexus_capture::default_backend() {
            Ok(b) => match b.monitors() {
                Ok(m) if !m.is_empty() => {
                    info!(
                        backend = b.name(),
                        monitors = m.len(),
                        "screen capture available"
                    );
                    Some(b)
                }
                Ok(_) => {
                    warn!("capture backend reports no monitors; video disabled");
                    None
                }
                Err(e) => {
                    warn!(error = %e, "capture backend unusable; video disabled");
                    None
                }
            },
            Err(e) => {
                warn!(error = %e, "no screen capture on this machine; video disabled");
                None
            }
        };
        let injector: Option<InjectorFactory> = if capture.is_some() {
            // Probe once so the log says whether control will work.
            match nexus_input::default_injector() {
                Ok(probe) => {
                    info!(backend = probe.name(), "input injection available");
                    drop(probe);
                    Some(Arc::new(nexus_input::default_injector))
                }
                Err(e) => {
                    warn!(error = %e, "input injection unavailable; sessions will be view-only");
                    None
                }
            }
        } else {
            None
        };
        Self::new(capture, injector, max_fps)
    }
}

#[async_trait]
impl SessionHandler for RemoteHandler {
    fn capabilities(&self) -> Capabilities {
        let mut features_list = vec![
            features::CURSOR_CHANNEL.to_owned(),
            features::DIRTY_RECTS.to_owned(),
        ];
        let mut caps = Capabilities {
            video_decode: nexus_codec::supported_decode()
                .into_iter()
                .map(|c| c as i32)
                .collect(),
            max_fps: self.max_fps,
            ..Default::default()
        };
        if self.capture.is_some() {
            caps.video_encode = nexus_codec::supported_encode()
                .into_iter()
                .map(|c| c as i32)
                .collect();
            features_list.push(features::MULTI_MONITOR.to_owned());
        }
        if self.injector.is_some() {
            features_list.push(features::INPUT_BASIC.to_owned());
        }
        if self.files.is_some() {
            features_list.push(features::FILES_RESUME.to_owned());
        }
        if self.power.is_some() {
            features_list.push(features::POWER_ACTIONS.to_owned());
        }
        if self.terminal.is_some() {
            features_list.push(features::TERMINAL_PTY.to_owned());
        }
        if self.portfwd.is_some() {
            features_list.push(features::PORT_FORWARD.to_owned());
        }
        caps.features = features_list;
        caps
    }

    fn clipboard(&self) -> Option<ClipboardSetup> {
        self.clipboard.clone()
    }

    async fn handle(&self, ctx: SessionContext) {
        let capture = match self.capture.clone() {
            Some(c) => c,
            None if self.serves_without_a_screen() => Arc::new(HeadlessBackend),
            None => {
                nexus_net::diag::serve(ctx.session, ctx.established.control).await;
                return;
            }
        };
        let config = TargetConfig {
            grants: ctx.peer.grants_from_peer.clone(),
            capture,
            injector: self.injector.clone(),
            max_fps: self.max_fps,
            max_inflight: target::DEFAULT_MAX_INFLIGHT,
            egress: None,
            files: self.files.clone(),
            clipboard: self.clipboard.clone(),
            consent: crate::consent::gate(&ctx.shared, &ctx.peer, ctx.session.path().kind),
            audit: Some(nexus_media::target::AuditContext {
                sink: Arc::clone(&ctx.shared.audit),
                actor: ctx.peer.identity.device_id().to_string(),
            }),
            power: self.power.clone(),
            wake_relay: self.wake_relay,
            terminal: self.terminal.clone(),
            portfwd: self.portfwd.clone(),
        };
        let stats = Arc::new(TargetStats::default());
        let reason = target::serve(ctx.session, ctx.established.control, config, stats).await;
        info!(peer = %ctx.peer.name, ?reason, "remote session finished");
    }
}
