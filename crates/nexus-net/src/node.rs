//! The local network endpoint: dials paired peers and accepts their connections.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::{Connection, IncomingAddr, WeakConnectionHandle, presets};
use iroh::tls::CaTlsConfig;
use iroh::{Endpoint, EndpointAddr, PublicKey, RelayMode, RelayUrl, SecretKey};
use iroh_relay::{RelayConfig, RelayMap, RelayQuicConfig};
use nexus_crypto::DeviceIdentity;
use parking_lot::Mutex;
use rustls_pki_types::CertificateDer;
use tokio::sync::{Semaphore, mpsc};
use tracing::{debug, info};

use crate::NetError;
use crate::allow::{AllowList, CLOSE_REVOKED, SESSION_ALPN};
use crate::config::{NodeConfig, RelaySetting, RelayTrust};
use crate::session::{PeerAddr, Session};

type Key = [u8; 32];
type Registry = Arc<Mutex<HashMap<Key, Vec<WeakConnectionHandle>>>>;

/// How long a peer gets to finish the TLS/QUIC handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long [`Node::connect`] waits for a connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Where this node can currently be reached, for publishing in presence.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Reachability {
    /// Home relay URL, if connected to one.
    pub relay: Option<String>,
    /// Direct socket addresses.
    pub direct: Vec<SocketAddr>,
}

/// A device's network presence.
pub struct Node {
    endpoint: Endpoint,
    allow: AllowList,
    sessions: Registry,
    incoming: tokio::sync::Mutex<mpsc::Receiver<Session>>,
}

impl std::fmt::Debug for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Node")
            .field("id", &format_args!("{}", self.endpoint.id().fmt_short()))
            .finish_non_exhaustive()
    }
}

fn build_error(e: impl std::fmt::Display) -> NetError {
    NetError::Bind(e.to_string())
}

impl Node {
    /// Starts a node whose network identity *is* the device identity (same Ed25519 key), so the
    /// peer you dial is cryptographically the device you paired with.
    pub async fn start(
        identity: &DeviceIdentity,
        config: &NodeConfig,
        allow: AllowList,
    ) -> Result<Self, NetError> {
        let secret = SecretKey::from_bytes(&identity.ed25519_secret_bytes());
        let transport = iroh::endpoint::QuicTransportConfig::builder()
            .max_idle_timeout(Some(
                config
                    .idle_timeout
                    .try_into()
                    .map_err(|_| build_error("idle timeout out of range"))?,
            ))
            .keep_alive_interval(config.keep_alive)
            .datagram_receive_buffer_size(Some(1 << 20))
            .datagram_send_buffer_size(1 << 20)
            .build();

        let mut builder = Endpoint::builder(presets::Minimal)
            .secret_key(secret)
            .alpns(vec![SESSION_ALPN.to_vec()])
            .hooks(allow.hook())
            .transport_config(transport);

        builder = match &config.relay {
            RelaySetting::Disabled => builder,
            RelaySetting::PublicDefault => builder.relay_mode(RelayMode::Default),
            RelaySetting::Custom(servers) => {
                let mut configs = Vec::with_capacity(servers.len());
                for server in servers {
                    let url = server
                        .url
                        .parse::<RelayUrl>()
                        .map_err(|e| build_error(format!("relay `{}`: {e}", server.url)))?;
                    let quic = server
                        .quic_port
                        .map_or_else(RelayQuicConfig::default, RelayQuicConfig::new);
                    let mut relay = RelayConfig::new(url, Some(quic));
                    if let Some(token) = &server.auth_token {
                        relay = relay.with_auth_token(token.clone());
                    }
                    configs.push(relay);
                }
                builder.relay_mode(RelayMode::Custom(configs.into_iter().collect::<RelayMap>()))
            }
        };
        builder = match &config.relay_trust {
            RelayTrust::System => builder.ca_tls_config(CaTlsConfig::system()),
            RelayTrust::CustomRoots(roots) => builder.ca_tls_config(CaTlsConfig::custom_roots(
                roots.iter().map(|der| CertificateDer::from(der.clone())),
            )),
        };
        if !config.direct {
            builder = builder.clear_ip_transports();
        } else if let Some(bind) = config.bind {
            builder = builder
                .clear_ip_transports()
                .bind_addr(bind)
                .map_err(build_error)?;
        }
        if !config.port_mapping {
            builder = builder.portmapper_config(iroh::endpoint::PortmapperConfig::Disabled);
        }

        let endpoint = builder.bind().await.map_err(build_error)?;
        info!(id = %endpoint.id().fmt_short(), "network endpoint started");

        let sessions: Registry = Arc::default();
        spawn_revocation_task(&allow, Arc::clone(&sessions));

        let (tx, rx) = mpsc::channel(64);
        spawn_accept_loop(
            endpoint.clone(),
            Arc::clone(&sessions),
            tx,
            config.max_pending_handshakes,
        );

        Ok(Self {
            endpoint,
            allow,
            sessions,
            incoming: tokio::sync::Mutex::new(rx),
        })
    }

    /// This node's public key (== the device's Ed25519 identity).
    #[must_use]
    pub fn id(&self) -> Key {
        *self.endpoint.id().as_bytes()
    }

    /// The live allow-list. Mutate it to pair/unpair devices.
    #[must_use]
    pub fn allow_list(&self) -> &AllowList {
        &self.allow
    }

    /// Where peers can currently reach this node.
    #[must_use]
    pub fn reachability(&self) -> Reachability {
        let addr = self.endpoint.addr();
        Reachability {
            relay: addr.relay_urls().next().map(ToString::to_string),
            direct: addr.ip_addrs().copied().collect(),
        }
    }

    /// Local socket addresses the node is bound to (useful on a fixed-port server or in tests).
    #[must_use]
    pub fn local_addrs(&self) -> Vec<SocketAddr> {
        self.endpoint.bound_sockets()
    }

    /// Waits until the node is connected to a home relay (if relays are configured).
    pub async fn wait_online(&self, limit: Duration) -> bool {
        tokio::time::timeout(limit, self.endpoint.online())
            .await
            .is_ok()
    }

    /// Dials `peer`. Fails immediately if it is not in the allow-list.
    pub async fn connect(&self, peer: &PeerAddr) -> Result<Session, NetError> {
        if !self.allow.contains(&peer.key) {
            return Err(NetError::NotPaired);
        }
        let id = PublicKey::from_bytes(&peer.key)
            .map_err(|_| NetError::Connect("invalid peer key".into()))?;
        let mut addr = EndpointAddr::new(id);
        if let Some(relay) = &peer.relay {
            let url = relay
                .parse::<RelayUrl>()
                .map_err(|e| NetError::Connect(format!("relay `{relay}`: {e}")))?;
            addr = addr.with_relay_url(url);
        }
        for direct in &peer.direct {
            addr = addr.with_ip_addr(*direct);
        }
        let conn = tokio::time::timeout(CONNECT_TIMEOUT, self.endpoint.connect(addr, SESSION_ALPN))
            .await
            .map_err(|_| NetError::Timeout)?
            .map_err(|e| NetError::Connect(e.to_string()))?;
        register(&self.sessions, &conn);
        debug!(peer = %conn.remote_id().fmt_short(), "outgoing session established");
        Ok(Session::new(conn, true))
    }

    /// Waits for the next authorised incoming session. `None` once the node is closed.
    pub async fn accept(&self) -> Option<Session> {
        self.incoming.lock().await.recv().await
    }

    /// Shuts the node down, closing every session.
    pub async fn close(&self) {
        self.endpoint.close().await;
    }
}

fn register(registry: &Registry, conn: &Connection) {
    let mut map = registry.lock();
    let list = map.entry(*conn.remote_id().as_bytes()).or_default();
    list.retain(|h| h.upgrade().is_some());
    list.push(conn.weak_handle());
}

/// When a peer is removed from the allow-list, terminate its live sessions at once.
fn spawn_revocation_task(allow: &AllowList, registry: Registry) {
    let mut rx = allow.subscribe_revocations();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(key) => {
                    let handles = registry.lock().remove(&key).unwrap_or_default();
                    for handle in handles {
                        if let Some(conn) = handle.upgrade() {
                            info!(peer = %conn.remote_id().fmt_short(), "closing session: pairing revoked");
                            conn.close(CLOSE_REVOKED.into(), b"revoked");
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // We may have missed revocations; the owner replaces the allow-list wholesale
                    // on any doubt, which re-broadcasts what was removed. Keep listening.
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

fn spawn_accept_loop(
    endpoint: Endpoint,
    registry: Registry,
    out: mpsc::Sender<Session>,
    max_pending: usize,
) {
    let pending = Arc::new(Semaphore::new(max_pending.max(1)));
    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            // Cheap pre-authentication defences, before any handshake work:
            //  * bound concurrent handshakes;
            //  * make direct (UDP) peers prove they own their source address (anti-spoofing).
            let Ok(permit) = Arc::clone(&pending).try_acquire_owned() else {
                incoming.refuse();
                continue;
            };
            if matches!(incoming.remote_addr(), IncomingAddr::Ip(_))
                && !incoming.remote_addr_validated()
            {
                let _ = incoming.retry();
                continue;
            }
            let registry = Arc::clone(&registry);
            let out = out.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let Ok(accepting) = incoming.accept() else {
                    return;
                };
                match tokio::time::timeout(HANDSHAKE_TIMEOUT, accepting).await {
                    Ok(Ok(conn)) => {
                        // The allow-list hook has already vetted the peer.
                        register(&registry, &conn);
                        debug!(peer = %conn.remote_id().fmt_short(), "incoming session established");
                        let _ = out.send(Session::new(conn, false)).await;
                    }
                    Ok(Err(e)) => debug!(error = %e, "incoming handshake failed or was rejected"),
                    Err(_) => debug!("incoming handshake timed out"),
                }
            });
        }
    });
}
