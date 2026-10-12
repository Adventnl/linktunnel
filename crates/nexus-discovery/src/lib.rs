//! Finding your own devices on the local network, with no server involved.
//!
//! Every agent advertises `_nexus-remote._udp` over mDNS and browses for the same. Plain mDNS
//! would announce "a Nexus device named X is here" to everyone on the Wi-Fi, which contradicts
//! the zero-trust rule that an unpaired device cannot even see another's presence. So the
//! advertisement carries no name and no device ID: its instance name is a short **tag**,
//! `BLAKE3-derive(device_id ‖ hour)[..8]`, which only someone who already knows the device ID
//! (that is, a paired peer) can compute, and which changes every hour so the same device cannot
//! be followed across hours or networks. A paired controller computes the tags of its paired
//! devices for the current, previous and next hour and matches them against what it hears.
//!
//! Discovery only yields *addresses*; the QUIC handshake still authenticates the peer's key
//! against the paired allow-list, so a spoofed advertisement can at worst waste a connection
//! attempt.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use parking_lot::Mutex;
use tracing::{debug, warn};

/// The mDNS service type.
pub const SERVICE_TYPE: &str = "_nexus-remote._udp.local.";
/// Seconds per tag epoch.
pub const EPOCH_SECS: u64 = 3600;
/// A heard advertisement is forgotten after this long without a refresh.
const FORGET_AFTER: Duration = Duration::from_secs(150);

/// Discovery failures.
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    /// The mDNS daemon could not start or a call to it failed.
    #[error("mDNS: {0}")]
    Mdns(String),
}

impl From<mdns_sd::Error> for DiscoveryError {
    fn from(e: mdns_sd::Error) -> Self {
        Self::Mdns(e.to_string())
    }
}

/// The 8-byte tag a device with `device_id` advertises during `epoch`.
#[must_use]
pub fn tag(device_id: &[u8], epoch: u64) -> [u8; 8] {
    let mut input = Vec::with_capacity(device_id.len() + 8);
    input.extend_from_slice(device_id);
    input.extend_from_slice(&epoch.to_le_bytes());
    let digest = blake3::derive_key("nexus-remote mdns tag v1", &input);
    let mut t = [0u8; 8];
    t.copy_from_slice(&digest[..8]);
    t
}

/// The current epoch number.
#[must_use]
pub fn epoch_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / EPOCH_SECS)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[derive(Debug, Clone)]
struct Heard {
    addrs: Vec<SocketAddr>,
    at: Instant,
}

/// Advertises this device and remembers what it hears from others.
pub struct Discovery {
    daemon: ServiceDaemon,
    heard: Arc<Mutex<HashMap<String, Heard>>>,
    advertised: Mutex<Option<(String, u64)>>,
    own: Mutex<Option<([u8; 16], u16)>>,
}

impl std::fmt::Debug for Discovery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Discovery")
            .field("heard", &self.heard.lock().len())
            .finish_non_exhaustive()
    }
}

impl Discovery {
    /// Starts the mDNS daemon and begins browsing.
    pub fn start() -> Result<Self, DiscoveryError> {
        let daemon = ServiceDaemon::new()?;
        let events = daemon.browse(SERVICE_TYPE)?;
        let heard: Arc<Mutex<HashMap<String, Heard>>> = Arc::default();
        let sink = Arc::clone(&heard);
        std::thread::Builder::new()
            .name("nexus-mdns".into())
            .spawn(move || {
                while let Ok(ev) = events.recv() {
                    match ev {
                        ServiceEvent::ServiceResolved(info) => {
                            let instance = info
                                .get_fullname()
                                .split('.')
                                .next()
                                .unwrap_or_default()
                                .to_owned();
                            let port = info.get_port();
                            let addrs: Vec<SocketAddr> = info
                                .get_addresses()
                                .iter()
                                .map(|a| SocketAddr::new(a.to_ip_addr(), port))
                                .collect();
                            debug!(%instance, ?addrs, "heard a Nexus advertisement");
                            sink.lock().insert(
                                instance,
                                Heard {
                                    addrs,
                                    at: Instant::now(),
                                },
                            );
                        }
                        ServiceEvent::ServiceRemoved(_, full) => {
                            let instance = full.split('.').next().unwrap_or_default().to_owned();
                            sink.lock().remove(&instance);
                        }
                        _ => {}
                    }
                }
            })
            .map_err(|e| DiscoveryError::Mdns(e.to_string()))?;
        Ok(Self {
            daemon,
            heard,
            advertised: Mutex::new(None),
            own: Mutex::new(None),
        })
    }

    /// Advertises this device (`device_id`) on `port`. Call [`refresh`](Self::refresh)
    /// periodically so the tag rolls over each hour.
    pub fn advertise(&self, device_id: [u8; 16], port: u16) -> Result<(), DiscoveryError> {
        *self.own.lock() = Some((device_id, port));
        self.refresh()
    }

    /// Re-registers the advertisement if the hour changed.
    pub fn refresh(&self) -> Result<(), DiscoveryError> {
        let Some((device_id, port)) = *self.own.lock() else {
            return Ok(());
        };
        let epoch = epoch_now();
        let mut current = self.advertised.lock();
        if current.as_ref().is_some_and(|(_, e)| *e == epoch) {
            return Ok(());
        }
        let instance = hex(&tag(&device_id, epoch));
        // The host name must not identify the machine either.
        let host = format!("{instance}.local.");
        let info = ServiceInfo::new(
            SERVICE_TYPE,
            &instance,
            &host,
            "",
            port,
            None::<HashMap<String, String>>,
        )?
        .enable_addr_auto();
        self.daemon.register(info)?;
        if let Some((old, _)) = current.take() {
            let full = format!("{old}.{SERVICE_TYPE}");
            if let Err(e) = self.daemon.unregister(&full) {
                warn!(error = %e, "could not withdraw the previous advertisement");
            }
        }
        *current = Some((instance, epoch));
        Ok(())
    }

    /// Addresses at which the device `device_id` was recently heard on this network.
    #[must_use]
    pub fn addresses_for(&self, device_id: &[u8]) -> Vec<SocketAddr> {
        let epoch = epoch_now();
        let wanted: Vec<String> = [epoch.wrapping_sub(1), epoch, epoch + 1]
            .iter()
            .map(|e| hex(&tag(device_id, *e)))
            .collect();
        let mut heard = self.heard.lock();
        heard.retain(|_, h| h.at.elapsed() < FORGET_AFTER);
        let mut out: Vec<SocketAddr> = wanted
            .iter()
            .filter_map(|t| heard.get(t))
            .flat_map(|h| h.addrs.iter().copied())
            // Link-local IPv6 needs a scope we do not carry; IPv4 and global IPv6 are enough.
            .filter(|a| !matches!(a.ip(), IpAddr::V6(v6) if (v6.segments()[0] & 0xffc0) == 0xfe80))
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Number of advertisements currently remembered (any device).
    #[must_use]
    pub fn heard_count(&self) -> usize {
        self.heard.lock().len()
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_stable_per_epoch_distinct_across_epochs_and_devices() {
        let (a, b) = ([1u8; 16], [2u8; 16]);
        assert_eq!(tag(&a, 7), tag(&a, 7));
        assert_ne!(tag(&a, 7), tag(&a, 8));
        assert_ne!(tag(&a, 7), tag(&b, 7));
    }

    #[test]
    fn the_tag_reveals_nothing_readable() {
        let t = hex(&tag(&[0xAB; 16], 1));
        assert_eq!(t.len(), 16);
        assert!(!t.contains("ab".repeat(8).as_str()));
    }
}
