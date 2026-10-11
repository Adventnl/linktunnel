//! The agent's view of its paired devices: trust data merged with live presence.

use std::collections::HashMap;

use nexus_proto::v1::{DevicePresence, DeviceView, PathKind, PresenceInfo, PresenceState};
use nexus_trust::PairedPeer;

pub(crate) type Key = [u8; 32];

#[derive(Debug, Clone)]
pub(crate) struct LivePresence {
    pub info: PresenceInfo,
    pub online: bool,
    pub last_seen_ms: u64,
}

/// Presence cache plus per-device session state.
#[derive(Debug, Default)]
pub(crate) struct DeviceTable {
    presence: HashMap<Key, LivePresence>,
    paths: HashMap<Key, PathKind>,
}

impl DeviceTable {
    pub fn update_presence(&mut self, key: Key, update: &DevicePresence) {
        if let Some(info) = &update.info {
            self.presence.insert(
                key,
                LivePresence {
                    info: info.clone(),
                    online: update.online,
                    last_seen_ms: update.last_seen_ms,
                },
            );
        }
    }

    pub fn presence(&self, key: &Key) -> Option<&LivePresence> {
        self.presence.get(key)
    }

    pub fn remove(&mut self, key: &Key) {
        self.presence.remove(key);
        self.paths.remove(key);
    }

    /// Everything we knew becomes "last known"; used when the signaling link drops.
    pub fn mark_all_unknown(&mut self) {
        for p in self.presence.values_mut() {
            p.online = false;
        }
    }

    pub fn set_path(&mut self, key: Key, path: Option<PathKind>) {
        match path {
            Some(p) => {
                self.paths.insert(key, p);
            }
            None => {
                self.paths.remove(&key);
            }
        }
    }

    /// Builds the dashboard view of a paired device.
    pub fn view(&self, peer: &PairedPeer) -> DeviceView {
        let key = peer.identity.ed25519_bytes();
        let live = self.presence.get(&key);
        let info = live.map(|l| &l.info);
        DeviceView {
            device_id: peer.identity.device_id().as_bytes().to_vec().into(),
            name: info
                .map(|i| i.name.clone())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| peer.name.clone()),
            state: info.map_or(PresenceState::Offline as i32, |i| {
                if live.is_some_and(|l| l.online) {
                    i.state
                } else {
                    offline_state(i.state)
                }
            }),
            online: live.is_some_and(|l| l.online),
            last_seen_ms: live.map_or(0, |l| l.last_seen_ms),
            os: info.and_then(|i| i.os.clone()),
            grants_on_peer: peer.grants_on_peer.iter().cloned().collect(),
            grants_from_peer: peer.grants_from_peer.iter().cloned().collect(),
            path: self
                .paths
                .get(&key)
                .copied()
                .unwrap_or(PathKind::Unspecified) as i32,
            active_user: info.map(|i| i.active_user.clone()).unwrap_or_default(),
            battery_percent: info.map_or(255, |i| i.battery_percent),
            fingerprint: peer.identity.fingerprint().to_string(),
        }
    }
}

/// An offline device keeps "sleeping"/"hibernating" (it can be woken) but never claims to be
/// online/idle/locked/in-use.
fn offline_state(state: i32) -> i32 {
    match PresenceState::try_from(state) {
        Ok(PresenceState::Sleeping) => PresenceState::Sleeping as i32,
        Ok(PresenceState::Hibernating) => PresenceState::Hibernating as i32,
        _ => PresenceState::Offline as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_devices_never_claim_to_be_in_use() {
        assert_eq!(
            offline_state(PresenceState::InUse as i32),
            PresenceState::Offline as i32
        );
        assert_eq!(
            offline_state(PresenceState::Locked as i32),
            PresenceState::Offline as i32
        );
        assert_eq!(
            offline_state(PresenceState::Sleeping as i32),
            PresenceState::Sleeping as i32
        );
        assert_eq!(
            offline_state(PresenceState::Hibernating as i32),
            PresenceState::Hibernating as i32
        );
    }
}
