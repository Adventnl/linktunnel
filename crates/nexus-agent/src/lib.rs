//! The Nexus agent: the one process per device that owns the identity, the signaling
//! connection, the paired-device allow-list and every session, and serves its clients (CLI, tray,
//! app) over authenticated local IPC.

#![forbid(unsafe_code)]

mod auth;
mod config;
mod consent;
mod devices;
mod error;
mod files_bridge;
mod forward_bridge;
mod ipc_server;
mod limits;
mod pairing;
mod policy;
mod power_bridge;
mod remote;
mod sessions;
mod state;
mod supervisor;
pub mod sysinfo;
mod terminal_bridge;
mod viewer_bridge;
mod wakebook;
#[cfg(windows)]
mod wts_prompter;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
use std::time::{Duration, Instant};

use nexus_crypto::{DeviceId, DeviceIdentity};
use nexus_ipc::{IpcPaths, IpcServer};
use nexus_net::{AllowList, Node};
use nexus_trust::TrustStore;
use parking_lot::Mutex;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use tracing::{info, warn};

pub use config::AgentConfig;
pub use consent::ConsentPrompter;
pub use error::AgentError;
pub use remote::RemoteHandler;
pub use sessions::{
    CLOSE_DISCONNECTED, CLOSE_LOCKDOWN, CLOSE_RATE_LIMITED, DiagnosticsHandler, SessionContext,
    SessionHandler,
};
pub use state::Shared;
#[cfg(windows)]
pub use wts_prompter::WtsPrompter;

/// Why the agent could not start.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// Step-up credential store could not be loaded.
    #[error(transparent)]
    Auth(#[from] nexus_auth::AuthError),
    /// State directories could not be created.
    #[error("preparing state directories: {0}")]
    Paths(std::io::Error),
    /// The trust store is unreadable or has been tampered with.
    #[error("loading the trust store: {0}")]
    Trust(#[from] nexus_trust::TrustError),
    /// The network endpoint could not start.
    #[error("starting the network endpoint: {0}")]
    Net(#[from] nexus_net::NetError),
    /// The IPC endpoint could not be created (another agent already running?).
    #[error("starting local IPC: {0}")]
    Ipc(#[from] nexus_ipc::IpcError),
    /// The audit log cannot be opened. The agent does not run without one.
    #[error("opening the audit log: {0}")]
    Audit(#[from] nexus_audit::AuditError),
}

/// A running agent.
pub struct Agent {
    shared: Arc<Shared>,
    tasks: Vec<JoinHandle<()>>,
    ipc_paths: IpcPaths,
    #[cfg(windows)]
    _session_ui: Option<nexus_winservice::session_ui::SessionUi>,
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("device", &self.shared.identity.device_id())
            .finish_non_exhaustive()
    }
}

impl Agent {
    /// Starts the agent with `identity`, serving sessions with `handler`.
    pub async fn start(
        config: AgentConfig,
        identity: Arc<DeviceIdentity>,
        handler: Arc<dyn SessionHandler>,
    ) -> Result<Self, StartError> {
        config.paths.ensure().map_err(StartError::Paths)?;
        let trust = TrustStore::open(config.trust_file(), identity.public().clone())?;
        let locked = config.start_locked_down || config.lockdown_file().exists();
        let allow = AllowList::from_keys(if locked {
            Vec::new()
        } else {
            trust.allowlist()
        });
        let audit: Arc<dyn nexus_audit::AuditSink> = Arc::new(nexus_audit::AuditLog::open(
            &config.audit_dir(),
            Some(Arc::clone(&identity)),
        )?);
        audit.record(
            "agent.started",
            None,
            &[("version", env!("CARGO_PKG_VERSION"))],
        );
        let node = Node::start(&identity, &config.node, allow).await?;
        let ipc_paths = IpcPaths::in_dir(&config.paths.runtime_dir);
        let ipc = IpcServer::bind(ipc_paths.clone()).await?;

        let policy_file = config.policy_file();
        let wake_file = config.paths.data_dir.join("wake.json");
        let default_consent = config.default_consent;
        let (events, _) = broadcast::channel(256);
        let (shutdown, _) = watch::channel(false);
        let discovery = start_discovery(&config, &identity, &node);
        let auth_store = config.auth_store.clone().unwrap_or_else(|| {
            Arc::new(nexus_crypto::store::FileStore::new(
                config.paths.data_dir.join("keys"),
            ))
        });
        let verifier = nexus_auth::Verifier::new(auth_store)?;
        let shared = Arc::new(Shared {
            auth: Mutex::new(verifier),
            remote_auth: Mutex::default(),
            ready_auth: Mutex::default(),
            cfg: config,
            identity,
            trust: Mutex::new(trust),
            node,
            devices: Mutex::default(),
            signal: Mutex::new(None),
            events,
            lockdown: AtomicBool::new(locked),
            sessions: AtomicU32::new(0),
            active: Mutex::default(),
            next_session_id: AtomicU64::new(1),
            pairing: Mutex::new(None),
            handler,
            shutdown,
            next_pairing_id: AtomicU64::new(1),
            viewers: Mutex::default(),
            next_viewer_id: AtomicU64::new(1),
            policy: Mutex::new(policy::PolicyStore::load(policy_file, default_consent)),
            terminals: Mutex::default(),
            forwards: Mutex::default(),
            wakebook: Mutex::new(wakebook::WakeBook::load(wake_file)),
            audit,
            limits: Mutex::default(),
            consents: Mutex::default(),
            next_consent_id: AtomicU64::new(1),
            started: Instant::now(),
            discovery,
        });
        info!(device = %shared.identity.device_id(), locked, "agent starting");

        let mut tasks = vec![
            tokio::spawn(auth::expire(Arc::clone(&shared))),
            tokio::spawn(supervisor::run(Arc::clone(&shared))),
            tokio::spawn(sessions::accept_loop(Arc::clone(&shared))),
            tokio::spawn(ipc_server::serve(Arc::clone(&shared), ipc)),
        ];
        if shared.discovery.is_some() {
            let s = Arc::clone(&shared);
            tasks.push(tokio::spawn(async move {
                let mut shutdown = s.shutdown.subscribe();
                // The advertised tag changes every hour; check often enough to roll over on time.
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
                while !*shutdown.borrow() {
                    tokio::select! {
                        _ = tick.tick() => {},
                        _ = shutdown.changed() => return,
                    }
                    if let Some(d) = &s.discovery
                        && let Err(e) = d.refresh()
                    {
                        tracing::warn!(error = %e, "refreshing the LAN advertisement failed");
                    }
                }
            }));
        }
        #[cfg(windows)]
        let session_ui = {
            let state = Arc::downgrade(&shared);
            let disconnect = Arc::downgrade(&shared);
            let panic = Arc::downgrade(&shared);
            let label =
                nexus_core::i18n::tr(nexus_core::i18n::Locale::detect(), "session.local_active");
            match nexus_winservice::session_ui::SessionUi::start(
                label,
                move || {
                    state.upgrade().map_or((false, true), |s| {
                        (
                            s.sessions.load(std::sync::atomic::Ordering::SeqCst) != 0,
                            *s.shutdown.borrow(),
                        )
                    })
                },
                move || {
                    if let Some(shared) = disconnect.upgrade() {
                        sessions::disconnect(&shared, &[]);
                    }
                },
                move || {
                    if let Some(shared) = panic.upgrade() {
                        sessions::disconnect(&shared, &[]);
                        // A physical panic must take effect even if writing the marker fails.
                        shared
                            .lockdown
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                        shared.refresh_allowlist();
                        if let Err(e) = ipc_server::set_lockdown(&shared, true) {
                            warn!(error = %e, "could not persist locally requested lockdown");
                        }
                    }
                },
            ) {
                Ok(ui) => Some(ui),
                Err(e) => {
                    warn!(error = %e, "could not start Windows session indicator");
                    None
                }
            }
        };
        Ok(Self {
            shared,
            tasks,
            ipc_paths,
            #[cfg(windows)]
            _session_ui: session_ui,
        })
    }

    /// Where clients reach this agent.
    #[must_use]
    pub fn ipc_paths(&self) -> &IpcPaths {
        &self.ipc_paths
    }

    /// This device's ID.
    #[must_use]
    pub fn device_id(&self) -> DeviceId {
        self.shared.identity.device_id()
    }

    /// Shared state (for embedding applications and tests).
    #[must_use]
    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    /// Resolves when shutdown has been requested (IPC `Shutdown` or [`shutdown`](Self::shutdown)).
    pub async fn wait_for_shutdown(&self) {
        let mut rx = self.shared.shutdown.subscribe();
        while !*rx.borrow() {
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    /// Stops the agent: announces going offline, closes sessions, removes the IPC endpoint.
    pub async fn shutdown(self) {
        let _ = self.shared.shutdown.send_replace(true);
        for mut task in self.tasks {
            if tokio::time::timeout(Duration::from_secs(5), &mut task)
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
            }
        }
        self.shared.node.close().await;
    }
}

/// Starts mDNS advertising/browsing; a machine where it cannot start just runs without it.
fn start_discovery(
    config: &AgentConfig,
    identity: &nexus_crypto::DeviceIdentity,
    node: &Node,
) -> Option<nexus_discovery::Discovery> {
    if !config.lan_discovery {
        return None;
    }
    let started = nexus_discovery::Discovery::start().and_then(|d| {
        let addrs = node.local_addrs();
        let port = addrs
            .iter()
            .find(|a| a.is_ipv4())
            .or_else(|| addrs.first())
            .map_or(0, std::net::SocketAddr::port);
        if port != 0 {
            d.advertise(*identity.device_id().as_bytes(), port)?;
        }
        Ok(d)
    });
    match started {
        Ok(d) => {
            info!("LAN discovery on (paired devices can find this one without a server)");
            Some(d)
        }
        Err(e) => {
            warn!(error = %e, "LAN discovery unavailable");
            None
        }
    }
}
