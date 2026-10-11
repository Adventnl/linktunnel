//! Server configuration.

use std::time::Duration;

/// A token-bucket rate: `burst` tokens, refilled at `per_sec`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rate {
    /// Sustained rate.
    pub per_sec: f64,
    /// Bucket size (largest instantaneous burst).
    pub burst: u32,
}

impl Rate {
    /// A rate expressed per minute (burst = the per-minute count).
    #[must_use]
    pub fn per_minute(n: u32) -> Self {
        Self {
            per_sec: f64::from(n) / 60.0,
            burst: n,
        }
    }
}

/// Everything tunable about a signaling server.
#[derive(Debug, Clone)]
pub struct SignalConfig {
    /// The name clients sign into their login proof (normally the public `host[:port]`). A proof
    /// made for one audience is useless against any other server.
    pub audience: String,
    /// How often clients should send a heartbeat.
    pub heartbeat_interval: Duration,
    /// A device with no heartbeat/update for this long is marked offline.
    pub presence_ttl: Duration,
    /// How long a new connection may take to authenticate.
    pub auth_timeout: Duration,
    /// Close connections that send nothing (not even WebSocket pongs) for this long.
    pub idle_timeout: Duration,
    /// Hard cap on one WebSocket message.
    pub max_message_bytes: usize,
    /// How long a rendezvous room lives.
    pub rendezvous_ttl: Duration,
    /// Per-connection message rate.
    pub message_rate: Rate,
    /// Failed logins allowed per source IP.
    pub auth_failure_rate: Rate,
    /// Rendezvous joins allowed per device.
    pub rendezvous_join_rate: Rate,
    /// Sealed envelopes allowed per device.
    pub envelope_rate: Rate,
    /// Trust `X-Forwarded-For` for the client IP (only behind a reverse proxy you control).
    pub trust_forwarded_for: bool,
    /// Capacity of each connection's outbound queue; a client that cannot keep up is dropped.
    pub outbound_queue: usize,
}

impl SignalConfig {
    /// Sensible production defaults for `audience`.
    #[must_use]
    pub fn new(audience: impl Into<String>) -> Self {
        Self {
            audience: audience.into(),
            heartbeat_interval: Duration::from_secs(5),
            presence_ttl: Duration::from_secs(15),
            auth_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(60),
            max_message_bytes: nexus_proto::signal::MAX_SIGNAL_MESSAGE,
            rendezvous_ttl: Duration::from_secs(300),
            message_rate: Rate {
                per_sec: 50.0,
                burst: 100,
            },
            auth_failure_rate: Rate::per_minute(10),
            rendezvous_join_rate: Rate::per_minute(10),
            envelope_rate: Rate {
                per_sec: 5.0,
                burst: 30,
            },
            trust_forwarded_for: false,
            outbound_queue: 256,
        }
    }
}
