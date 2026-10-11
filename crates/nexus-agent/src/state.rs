//! State shared by every agent task.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use nexus_crypto::{DeviceId, DeviceIdentity};
use nexus_net::Node;
use nexus_proto::v1::{DeviceView, IpcEvent, Status, ipc_event::Body as Ev};
use nexus_signal_client::SignalClient;
use nexus_trust::{PairedPeer, TrustStore};
use parking_lot::Mutex;
use tokio::sync::{broadcast, mpsc, watch};

use crate::config::AgentConfig;
use crate::devices::{DeviceTable, Key};
use crate::sessions::SessionHandler;

/// A message from the signaling rendezvous destined for the pairing in progress.
#[derive(Debug)]
pub(crate) enum RendezvousMsg {
    Peer(bool),
    Data(Vec<u8>),
}

/// The one pairing that may be in progress.
pub(crate) struct PairingSlot {
    pub id: u64,
    pub confirm: mpsc::Sender<bool>,
    pub rendezvous: mpsc::Sender<RendezvousMsg>,
    pub cancel: watch::Sender<bool>,
}

/// Everything the agent's tasks share.
pub struct Shared {
    pub(crate) cfg: AgentConfig,
    pub(crate) identity: Arc<DeviceIdentity>,
    pub(crate) trust: Mutex<TrustStore>,
    pub(crate) node: Node,
    pub(crate) devices: Mutex<DeviceTable>,
    pub(crate) signal: Mutex<Option<SignalClient>>,
    pub(crate) events: broadcast::Sender<IpcEvent>,
    pub(crate) lockdown: AtomicBool,
    pub(crate) sessions: AtomicU32,
    pub(crate) pairing: Mutex<Option<PairingSlot>>,
    pub(crate) handler: Arc<dyn SessionHandler>,
    pub(crate) shutdown: watch::Sender<bool>,
    pub(crate) next_pairing_id: AtomicU64,
    pub(crate) viewers: Mutex<std::collections::HashMap<u64, crate::viewer_bridge::ViewerEntry>>,
    pub(crate) next_viewer_id: AtomicU64,
    pub(crate) policy: Mutex<crate::policy::PolicyStore>,
    pub(crate) terminals:
        Mutex<std::collections::HashMap<u64, crate::terminal_bridge::TerminalEntry>>,
    pub(crate) forwards: Mutex<std::collections::HashMap<u64, crate::forward_bridge::ForwardEntry>>,
    pub(crate) wakebook: Mutex<crate::wakebook::WakeBook>,
    pub(crate) consents: Mutex<std::collections::HashMap<u64, crate::consent::Pending>>,
    pub(crate) next_consent_id: AtomicU64,
    pub(crate) started: Instant,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared")
            .field("device", &self.identity.device_id())
            .finish_non_exhaustive()
    }
}

impl Shared {
    pub(crate) fn now_ms() -> u64 {
        nexus_core::clock::unix_ms()
    }

    /// Pushes an event to every connected IPC client.
    pub(crate) fn emit(&self, body: Ev) {
        // No receivers is normal (nobody is watching).
        let _ = self.events.send(IpcEvent { body: Some(body) });
    }

    pub(crate) fn is_locked_down(&self) -> bool {
        self.lockdown.load(Ordering::SeqCst)
    }

    /// The signaling client, if currently connected.
    pub(crate) fn signal(&self) -> Option<SignalClient> {
        self.signal.lock().clone()
    }

    /// Dashboard views of every paired device.
    pub(crate) fn device_views(&self) -> Vec<DeviceView> {
        let trust = self.trust.lock();
        let devices = self.devices.lock();
        let policy = self.policy.lock();
        let mut views: Vec<DeviceView> = trust
            .peers()
            .map(|p| {
                let mut v = devices.view(p);
                v.unattended = policy.is_unattended(&p.identity.ed25519_bytes());
                v
            })
            .collect();
        views.sort_by_key(|v| v.name.to_lowercase());
        views
    }

    /// Tells IPC clients that `key`'s view changed.
    pub(crate) fn emit_device_changed(&self, key: &Key) {
        let view = {
            let trust = self.trust.lock();
            let devices = self.devices.lock();
            let unattended = self.policy.lock().is_unattended(key);
            trust.get(key).map(|p| {
                let mut v = devices.view(p);
                v.unattended = unattended;
                v
            })
        };
        if let Some(device) = view {
            self.emit(Ev::DeviceChanged(nexus_proto::v1::DeviceChanged {
                device: Some(device),
            }));
        }
    }

    /// Re-derives the transport allow-list from the trust store (empty during lockdown).
    pub(crate) fn refresh_allowlist(&self) {
        let keys = if self.is_locked_down() {
            Vec::new()
        } else {
            self.trust.lock().allowlist()
        };
        self.node.allow_list().replace(keys);
    }

    pub(crate) fn find_peer(&self, device_id: &[u8]) -> Option<PairedPeer> {
        let wanted = DeviceId::from_slice(device_id).ok()?;
        self.trust
            .lock()
            .peers()
            .find(|p| p.identity.device_id() == wanted)
            .cloned()
    }

    pub(crate) fn status(&self) -> Status {
        let reach = self.node.reachability();
        Status {
            device_id: self.identity.device_id().as_bytes().to_vec().into(),
            device_name: self.cfg.device_name.clone(),
            fingerprint: self.identity.public().fingerprint().to_string(),
            agent_version: env!("CARGO_PKG_VERSION").to_owned(),
            signal_connected: self.signal.lock().is_some(),
            signal_url: self
                .cfg
                .signal_url
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            relay_url: reach.relay.unwrap_or_default(),
            direct_addresses: reach.direct.iter().map(ToString::to_string).collect(),
            lockdown: self.is_locked_down(),
            paired_devices: u32::try_from(self.trust.lock().peers().count()).unwrap_or(u32::MAX),
            active_sessions: self.sessions.load(Ordering::SeqCst),
        }
    }
}
