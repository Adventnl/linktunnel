//! The target-side session handler: serves remote-desktop sessions (and diagnostics).

use std::sync::Arc;

use async_trait::async_trait;
use nexus_capture::CaptureBackend;
use nexus_media::target::{self, InjectorFactory, TargetConfig, TargetStats};
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
}

impl std::fmt::Debug for RemoteHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteHandler")
            .field("capture", &self.capture.as_ref().map(|c| c.name()))
            .field("input", &self.injector.is_some())
            .field("max_fps", &self.max_fps)
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
        }
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
        caps.features = features_list;
        caps
    }

    async fn handle(&self, ctx: SessionContext) {
        let Some(capture) = self.capture.clone() else {
            nexus_net::diag::serve(ctx.session, ctx.established.control).await;
            return;
        };
        let config = TargetConfig {
            grants: ctx.peer.grants_from_peer.clone(),
            capture,
            injector: self.injector.clone(),
            max_fps: self.max_fps,
            max_inflight: target::DEFAULT_MAX_INFLIGHT,
            egress: None,
        };
        let stats = Arc::new(TargetStats::default());
        let reason = target::serve(ctx.session, ctx.established.control, config, stats).await;
        info!(peer = %ctx.peer.name, ?reason, "remote session finished");
    }
}
