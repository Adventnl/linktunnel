//! Agent configuration.

use std::path::PathBuf;
use std::time::Duration;

use nexus_core::NexusConfig;
use nexus_core::config::ConsentMode;
use nexus_core::paths::Paths;
use nexus_net::NodeConfig;
use url::Url;

/// Everything the agent needs to start.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Human-readable name shown on other devices (defaults to the host name).
    pub device_name: String,
    /// Where state lives.
    pub paths: Paths,
    /// Signaling server. `None` runs the agent LAN-only (no presence, no pairing registry).
    pub signal_url: Option<Url>,
    /// Network endpoint settings.
    pub node: NodeConfig,
    /// Maximum delay between signaling reconnect attempts.
    pub reconnect_max: Duration,
    /// Also publish loopback addresses as reachability hints (tests and local development only).
    pub publish_loopback_hints: bool,
    /// Start in lockdown regardless of the persisted flag (tests).
    pub start_locked_down: bool,
    /// Whether a person must approve each session of a paired device that has not been
    /// pre-authorised (the default for new pairings).
    pub default_consent: ConsentMode,
    /// How long that prompt waits before the request is refused.
    pub consent_timeout: Duration,
    /// Reads idle / locked / battery state for presence; `None` uses the operating system's.
    pub presence_sensor: Option<std::sync::Arc<dyn nexus_presence::Sensor>>,
    /// How often activity (idle, locked, battery) is re-read.
    pub presence_interval: Duration,
}

impl AgentConfig {
    /// Builds an agent config from the application config.
    pub fn from_nexus_config(
        cfg: &NexusConfig,
        paths: Paths,
        device_name: String,
    ) -> Result<Self, nexus_net::NetError> {
        Ok(Self {
            device_name,
            signal_url: cfg.net.signal_url.clone(),
            node: NodeConfig::from_net_config(&cfg.net)?,
            paths,
            reconnect_max: Duration::from_secs(30),
            publish_loopback_hints: false,
            start_locked_down: cfg.security.lockdown,
            default_consent: cfg.security.consent,
            consent_timeout: Duration::from_secs(u64::from(cfg.security.consent_timeout_secs)),
            presence_sensor: None,
            presence_interval: Duration::from_secs(10),
        })
    }

    /// Location of the trust store file.
    #[must_use]
    pub fn trust_file(&self) -> PathBuf {
        self.paths.data_dir.join("trust.json")
    }

    /// The tamper-evident audit log.
    #[must_use]
    pub fn audit_dir(&self) -> PathBuf {
        self.paths.data_dir.join("audit")
    }

    /// Per-device consent overrides.
    #[must_use]
    pub fn policy_file(&self) -> PathBuf {
        self.paths.data_dir.join("policy.json")
    }

    /// Marker file whose presence means "lockdown is on" (survives restarts).
    #[must_use]
    pub fn lockdown_file(&self) -> PathBuf {
        self.paths.data_dir.join("lockdown")
    }
}
