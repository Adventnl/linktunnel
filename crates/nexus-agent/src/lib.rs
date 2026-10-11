//! The Nexus agent: the one process per device that owns the identity, the signaling
//! connection, the paired-device allow-list and every session, and serves its clients (CLI, tray,
//! app) over authenticated local IPC.

#![forbid(unsafe_code)]

mod config;
mod consent;
mod devices;
mod error;
mod files_bridge;
mod ipc_server;
mod pairing;
mod policy;
mod remote;
mod sessions;
mod state;
mod supervisor;
pub mod sysinfo;
mod viewer_bridge;

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
use tracing::info;

pub use config::AgentConfig;
pub use error::AgentError;
pub use remote::RemoteHandler;
pub use sessions::{CLOSE_LOCKDOWN, DiagnosticsHandler, SessionContext, SessionHandler};
pub use state::Shared;

/// Why the agent could not start.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
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
}

/// A running agent.
pub struct Agent {
    shared: Arc<Shared>,
    tasks: Vec<JoinHandle<()>>,
    ipc_paths: IpcPaths,
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
        let node = Node::start(&identity, &config.node, allow).await?;
        let ipc_paths = IpcPaths::in_dir(&config.paths.runtime_dir);
        let ipc = IpcServer::bind(ipc_paths.clone()).await?;

        let policy_file = config.policy_file();
        let default_consent = config.default_consent;
        let (events, _) = broadcast::channel(256);
        let (shutdown, _) = watch::channel(false);
        let shared = Arc::new(Shared {
            cfg: config,
            identity,
            trust: Mutex::new(trust),
            node,
            devices: Mutex::default(),
            signal: Mutex::new(None),
            events,
            lockdown: AtomicBool::new(locked),
            sessions: AtomicU32::new(0),
            pairing: Mutex::new(None),
            handler,
            shutdown,
            next_pairing_id: AtomicU64::new(1),
            viewers: Mutex::default(),
            next_viewer_id: AtomicU64::new(1),
            policy: Mutex::new(policy::PolicyStore::load(policy_file, default_consent)),
            consents: Mutex::default(),
            next_consent_id: AtomicU64::new(1),
            started: Instant::now(),
        });
        info!(device = %shared.identity.device_id(), locked, "agent starting");

        let tasks = vec![
            tokio::spawn(supervisor::run(Arc::clone(&shared))),
            tokio::spawn(sessions::accept_loop(Arc::clone(&shared))),
            tokio::spawn(ipc_server::serve(Arc::clone(&shared), ipc)),
        ];
        Ok(Self {
            shared,
            tasks,
            ipc_paths,
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
        let _ = self.shared.shutdown.send(true);
        for task in self.tasks {
            let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
        }
        self.shared.node.close().await;
    }
}
