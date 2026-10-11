//! Node configuration.

use std::net::SocketAddr;
use std::time::Duration;

use nexus_core::config::NetConfig;

use crate::NetError;

/// Which relay servers a node may use for hole-punching assistance and as a last-resort path.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RelaySetting {
    /// No relay: only direct connectivity (LAN, public/IPv6 addresses, port mappings).
    /// This is the default so that nothing is routed through third-party infrastructure unless
    /// the owner chose to.
    #[default]
    Disabled,
    /// Self-hosted relays.
    Custom(Vec<RelayServer>),
    /// The public relays operated by the iroh project. Traffic stays end-to-end encrypted, but
    /// metadata (which keys talk, when) is visible to that operator. Opt-in only.
    PublicDefault,
}

/// A self-hosted relay server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayServer {
    /// `https://relay.example.com`.
    pub url: String,
    /// UDP port of the relay's QUIC address-discovery service (defaults to 7842 when `None`);
    /// it lets nodes learn their public address for hole punching.
    pub quic_port: Option<u16>,
    /// Bearer token presented to the relay if it restricts who may use it.
    pub auth_token: Option<String>,
}

impl RelayServer {
    /// A relay reachable at `url` with default QUIC discovery and no token.
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            quic_port: None,
            auth_token: None,
        }
    }
}

/// How the relay's TLS certificate is verified. (This protects only the connection to the relay;
/// session content is end-to-end encrypted regardless, and the relay never sees plaintext.)
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RelayTrust {
    /// The operating system's trust store (production, publicly signed relay certificates).
    #[default]
    System,
    /// Trust only these DER-encoded root/leaf certificates: a self-hosted relay with a private CA
    /// or a pinned self-signed certificate.
    CustomRoots(Vec<Vec<u8>>),
}

/// Everything needed to start a [`crate::Node`].
#[derive(Debug, Clone)]
pub struct NodeConfig {
    /// Fixed local address to bind (servers pin a UDP port so a firewall rule can allow it).
    /// `None` binds ephemeral ports on all interfaces.
    pub bind: Option<SocketAddr>,
    /// Allow direct (UDP) paths. `false` forces relay-only (tests, locked-down networks).
    pub direct: bool,
    /// Relay selection.
    pub relay: RelaySetting,
    /// Relay certificate verification.
    pub relay_trust: RelayTrust,
    /// Ask the router for a port mapping (UPnP / NAT-PMP / PCP) to improve direct connectivity.
    pub port_mapping: bool,
    /// Close a session after this long without any traffic.
    pub idle_timeout: Duration,
    /// QUIC keep-alive interval.
    pub keep_alive: Duration,
    /// Most handshakes processed at once (a cheap bound on pre-authentication work).
    pub max_pending_handshakes: usize,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            bind: None,
            direct: true,
            relay: RelaySetting::Disabled,
            relay_trust: RelayTrust::System,
            port_mapping: true,
            idle_timeout: Duration::from_secs(30),
            keep_alive: Duration::from_secs(5),
            max_pending_handshakes: 32,
        }
    }
}

impl NodeConfig {
    /// Builds a config from the shared application configuration.
    pub fn from_net_config(cfg: &NetConfig) -> Result<Self, NetError> {
        let relay = if cfg.relay_urls.is_empty() {
            RelaySetting::Disabled
        } else {
            RelaySetting::Custom(
                cfg.relay_urls
                    .iter()
                    .map(|u| RelayServer::new(u.to_string()))
                    .collect(),
            )
        };
        let bind = (cfg.udp_port != 0)
            .then(|| SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, cfg.udp_port)));
        Ok(Self {
            bind,
            relay,
            ..Self::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_never_use_third_party_infrastructure() {
        let cfg = NodeConfig::default();
        assert_eq!(cfg.relay, RelaySetting::Disabled);
        assert_eq!(cfg.relay_trust, RelayTrust::System);
    }

    #[test]
    fn from_net_config_maps_relays_and_port() {
        let mut net = NetConfig::default();
        assert_eq!(
            NodeConfig::from_net_config(&net).unwrap().relay,
            RelaySetting::Disabled
        );
        net.udp_port = 7842;
        net.relay_urls = vec!["https://relay.example.com".parse().unwrap()];
        let cfg = NodeConfig::from_net_config(&net).unwrap();
        assert_eq!(cfg.bind.unwrap().port(), 7842);
        assert!(matches!(cfg.relay, RelaySetting::Custom(ref v) if v.len() == 1));
    }
}
