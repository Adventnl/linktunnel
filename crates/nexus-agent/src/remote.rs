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
    audio: Option<target::AudioSourceFactory>,
    privacy: Option<Arc<dyn nexus_privacy::PrivacyBackend>>,
    /// Keeps a virtual screen alive for as long as this handler serves it.
    _vdisplay: Option<Arc<parking_lot::Mutex<nexus_vdisplay::VirtualDisplay>>>,
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
            audio: None,
            privacy: nexus_privacy::detect(),
            _vdisplay: None,
        }
    }

    /// Installs an explicit privacy backend (embedding and lifecycle tests).
    #[must_use]
    pub fn with_privacy(mut self, backend: Arc<dyn nexus_privacy::PrivacyBackend>) -> Self {
        self.privacy = Some(backend);
        self
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

    /// Lets controllers holding the `audio` grant hear what this machine plays.
    #[must_use]
    pub fn with_audio(mut self, source: target::AudioSourceFactory) -> Self {
        self.audio = Some(source);
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
            || self.audio.is_some()
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

    /// Like [`detect`](Self::detect), but gives a screen-less Linux machine a virtual one
    /// according to `cfg` (see [`nexus_core::config::DisplayConfig`]).
    #[must_use]
    pub fn detect_with_display(max_fps: u32, cfg: &nexus_core::config::DisplayConfig) -> Self {
        use nexus_core::config::VirtualScreen;
        let real = match cfg.virtual_screen {
            VirtualScreen::Always => Self::new(None, None, max_fps),
            _ => Self::detect(max_fps),
        };
        if cfg.virtual_screen == VirtualScreen::Never || real.capture.is_some() {
            return real;
        }
        match Self::start_virtual(max_fps, cfg) {
            Ok(h) => h,
            Err(e) => {
                warn!(error = %e, "no virtual screen; this machine serves terminal, files and ports only");
                real
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn start_virtual(
        max_fps: u32,
        cfg: &nexus_core::config::DisplayConfig,
    ) -> Result<Self, String> {
        let vcfg = nexus_vdisplay::VirtualDisplayConfig {
            dpi: cfg.dpi,
            session: cfg.session.clone(),
            ..nexus_vdisplay::VirtualDisplayConfig::default()
        }
        .with_size(&cfg.size)
        .map_err(|e| e.to_string())?;
        let vd = nexus_vdisplay::VirtualDisplay::start(&vcfg).map_err(|e| e.to_string())?;
        let name = vd.name().to_owned();
        let backend =
            nexus_capture::x11::X11Backend::connect(Some(&name)).map_err(|e| e.to_string())?;
        info!(display = %name, size = %cfg.size, "serving a virtual screen");
        let injector_display = name.clone();
        let injector: InjectorFactory = Arc::new(move || {
            nexus_input::x11::X11Injector::connect(Some(&injector_display))
                .map(|i| Box::new(i) as Box<dyn nexus_input::Injector>)
        });
        let mut h = Self::new(Some(Arc::new(backend)), Some(injector), max_fps);
        h._vdisplay = Some(Arc::new(parking_lot::Mutex::new(vd)));
        Ok(h)
    }

    #[cfg(not(target_os = "linux"))]
    fn start_virtual(
        _max_fps: u32,
        _cfg: &nexus_core::config::DisplayConfig,
    ) -> Result<Self, String> {
        Err("virtual screens are only available on Linux".into())
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
        if self.audio.is_some() {
            features_list.push(features::AUDIO_OPUS.to_owned());
        }
        if self.privacy.is_some() {
            features_list.push(features::PRIVACY_SCREEN.to_owned());
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
            encoder: None,
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
            power: self.power.clone().map(|inner| {
                Arc::new(AnnouncingPower {
                    inner,
                    shared: Arc::clone(&ctx.shared),
                    runtime: tokio::runtime::Handle::current(),
                }) as Arc<dyn PowerBackend>
            }),
            wake_relay: self.wake_relay,
            audio: self.audio.clone(),
            privacy: self.privacy.clone(),
            terminal: self.terminal.clone(),
            portfwd: self.portfwd.clone(),
        };
        let stats = Arc::new(TargetStats::default());
        let reason = target::serve(ctx.session, ctx.established.control, config, stats).await;
        info!(peer = %ctx.peer.name, ?reason, "remote session finished");
        // A dropped link may come straight back without a new prompt; a goodbye, a refusal or a
        // local stop may not.
        if !matches!(reason, target::EndReason::ConnectionLost(_)) {
            ctx.shared
                .limits
                .lock()
                .ended_on_purpose(&ctx.peer.identity.ed25519_bytes());
        }
    }
}

/// Tells the signaling server why this device is about to disappear, so paired devices see
/// "sleeping" or "restarting" at once instead of "offline" after a timeout.
struct AnnouncingPower {
    inner: Arc<dyn PowerBackend>,
    shared: Arc<crate::state::Shared>,
    runtime: tokio::runtime::Handle,
}

impl PowerBackend for AnnouncingPower {
    fn perform(&self, action: nexus_power::PowerAction) -> Result<(), nexus_power::PowerError> {
        use nexus_power::PowerAction as A;
        use nexus_proto::v1::GoingOfflineReason as R;
        let reason = match action {
            A::Sleep => Some(R::Sleep),
            A::Hibernate => Some(R::Hibernate),
            A::Restart => Some(R::Restart),
            A::Shutdown => Some(R::Shutdown),
            A::LogOff => Some(R::Logout),
            A::Lock => None,
        };
        if let (Some(reason), Some(signal)) = (reason, self.shared.signal()) {
            // Runs on a blocking thread, so waiting on the runtime here is fine.
            let _ = self.runtime.block_on(tokio::time::timeout(
                std::time::Duration::from_secs(2),
                signal.going_offline(reason),
            ));
        }
        self.inner.perform(action)
    }

    fn supported(&self) -> Vec<nexus_power::PowerAction> {
        self.inner.supported()
    }
}
