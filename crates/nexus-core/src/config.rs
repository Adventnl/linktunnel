//! Layered configuration.
//!
//! Precedence, lowest to highest:
//!
//! 1. compiled-in defaults ([`NexusConfig::default`]),
//! 2. `config.toml` (see [`crate::paths::Paths::config_file`]),
//! 3. environment variables prefixed `NEXUS_`, with `__` as the nesting separator
//!    (`NEXUS_LOG__LEVEL=debug`, `NEXUS_NET__UDP_PORT=7842`).
//!
//! Unknown keys are rejected so typos never silently disable a security setting.

use std::path::{Path, PathBuf};

use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use serde::{Deserialize, Serialize};
use url::Url;

/// Environment variable prefix for configuration overrides.
pub const ENV_PREFIX: &str = "NEXUS_";

/// Errors loading or validating configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file or environment could not be parsed into [`NexusConfig`].
    #[error("invalid configuration: {0}")]
    Parse(Box<figment::Error>),
    /// The configuration parsed but a value is not acceptable.
    #[error("invalid configuration value for `{key}`: {reason}")]
    Invalid {
        /// Dotted key path, e.g. `net.relay_urls`.
        key: &'static str,
        /// Human-readable reason.
        reason: String,
    },
}

impl From<figment::Error> for ConfigError {
    fn from(e: figment::Error) -> Self {
        Self::Parse(Box::new(e))
    }
}

/// Top-level configuration shared by every Nexus binary.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct NexusConfig {
    /// Logging.
    pub log: LogConfig,
    /// Networking.
    pub net: NetConfig,
    /// Security and consent policy defaults.
    pub security: SecurityConfig,
    /// Folders other devices may browse and copy files in and out of.
    pub files: FilesConfig,
    /// Clipboard sync.
    pub clipboard: ClipboardConfig,
    /// Remote power control and Wake-on-LAN.
    pub power: PowerConfig,
    /// Remote terminals.
    pub terminal: TerminalConfig,
    /// TCP port forwarding.
    pub portfwd: PortForwardConfig,
    /// Software updates.
    pub update: UpdateConfig,
}

/// Where updates come from and whom to trust. With no publisher key configured, updates are
/// disabled entirely (never "accept anything").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct UpdateConfig {
    /// URL of the signed release manifest.
    pub manifest_url: Option<Url>,
    /// Hex Ed25519 public keys of trusted publishers (`nexus-release keygen` prints one).
    pub public_keys: Vec<String>,
    /// `stable` or `beta`.
    pub channel: String,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            manifest_url: None,
            public_keys: Vec::new(),
            channel: "stable".into(),
        }
    }
}

/// One destination a controller may forward to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForwardTarget {
    /// Host name or IP address (`*` = any).
    pub host: String,
    /// First allowed port.
    pub port_min: u16,
    /// Last allowed port (defaults to `port_min`).
    #[serde(default)]
    pub port_max: Option<u16>,
}

/// Port forwarding: controllers holding the `terminal` grant may open TCP connections from this
/// device. By default only to this device's own loopback addresses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PortForwardConfig {
    /// Offer forwarding at all.
    pub enabled: bool,
    /// Allow any port on this device's loopback addresses.
    pub loopback: bool,
    /// Further destinations (other machines on this device's network).
    pub allow: Vec<ForwardTarget>,
    /// Most simultaneous forwarded connections.
    pub max_connections: u32,
}

impl Default for PortForwardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            loopback: true,
            allow: Vec::new(),
            max_connections: 64,
        }
    }
}

/// Remote shells. Using one also needs the `terminal` grant from the pairing and, unless the
/// device is pre-authorised, a person's approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TerminalConfig {
    /// Offer terminals at all.
    pub enabled: bool,
    /// Programs a controller may ask for besides the default shell (full paths).
    pub shells: Vec<String>,
    /// Hours a persistent terminal is kept after its controller disconnects.
    pub detached_ttl_hours: u32,
    /// Most terminals at once.
    pub max_sessions: u32,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            shells: Vec::new(),
            detached_ttl_hours: 24,
            max_sessions: 16,
        }
    }
}

/// What other devices may do to this machine's power state, and whether this machine helps wake
/// others. Everything also needs the matching grant from the pairing and, unless the device is
/// pre-authorised, a person's approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PowerConfig {
    /// Accept lock / sleep / restart / shut down requests.
    pub accept_requests: bool,
    /// Send Wake-on-LAN packets for devices that hold the `wake` grant (run this on an
    /// always-on machine such as a home server).
    pub wake_relay: bool,
}

impl Default for PowerConfig {
    fn default() -> Self {
        Self {
            accept_requests: true,
            wake_relay: false,
        }
    }
}

/// Clipboard sync between paired devices that hold the `clipboard` grant. The two directions are
/// separate switches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ClipboardConfig {
    /// Send what is copied on this device to the other one.
    pub send: bool,
    /// Apply what is copied on the other device.
    pub receive: bool,
    /// Include images (text only otherwise).
    pub images: bool,
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            send: true,
            receive: true,
            images: true,
        }
    }
}

/// One folder shared with paired devices that hold the `files` grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedFolder {
    /// The folder on this device.
    pub path: PathBuf,
    /// May a controller create, overwrite and delete inside it? Read-only when false.
    #[serde(default)]
    pub writable: bool,
}

/// File sharing. Nothing is shared until a folder is listed here (or given with `--share`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct FilesConfig {
    /// Folders to share.
    pub shared: Vec<SharedFolder>,
    /// Largest single upload this device accepts, in MiB.
    pub max_upload_mib: u64,
}

impl Default for FilesConfig {
    fn default() -> Self {
        Self {
            shared: Vec::new(),
            max_upload_mib: 1024 * 1024,
        }
    }
}

/// Log output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Multi-line, human friendly.
    Pretty,
    /// Single line, human friendly.
    #[default]
    Compact,
    /// One JSON object per line, for log shippers.
    Json,
}

/// Logging settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LogConfig {
    /// `tracing` filter directives, e.g. `info,nexus_net=debug`. `RUST_LOG` overrides it.
    pub level: String,
    /// Output format.
    pub format: LogFormat,
    /// Optional directory for daily-rotated log files (in addition to stderr).
    pub file_dir: Option<PathBuf>,
    /// Colour output on stderr.
    pub ansi: bool,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: "info".into(),
            format: LogFormat::Compact,
            file_dir: None,
            ansi: true,
        }
    }
}

/// Networking settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct NetConfig {
    /// WebSocket URL of the signaling/presence server (`wss://…/v1/ws`).
    pub signal_url: Option<Url>,
    /// Self-hosted relay URLs. Empty means "use the built-in default relay set".
    pub relay_urls: Vec<Url>,
    /// Local UDP port for direct connections. `0` picks an ephemeral port; servers should
    /// pin a port so a firewall rule can allow it.
    pub udp_port: u16,
    /// Announce/discover peers on the local network (mDNS).
    pub lan_discovery: bool,
    /// Permit a plain `ws://` signaling URL to a non-loopback host. **Trials on a trusted LAN
    /// only**: an on-path attacker could then observe metadata (who is paired, who is online) and
    /// drop or delay messages; sealed content, pairings and sessions stay protected by their own
    /// end-to-end cryptography.
    pub allow_insecure_signal: bool,
}

impl Default for NetConfig {
    fn default() -> Self {
        Self {
            signal_url: None,
            relay_urls: Vec::new(),
            udp_port: 0,
            lan_discovery: true,
            allow_insecure_signal: false,
        }
    }
}

/// How an incoming connection is authorised at the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConsentMode {
    /// A person at the target must click *Allow* (with a timeout).
    #[default]
    Attended,
    /// Paired controllers are pre-authorised (still subject to step-up authentication).
    Unattended,
}

/// Security defaults. Per-device overrides live in the agent's policy store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SecurityConfig {
    /// Default consent mode for new pairings.
    pub consent: ConsentMode,
    /// Seconds an attended prompt waits before the request is denied.
    pub consent_timeout_secs: u32,
    /// Require a second factor (TOTP / passkey) for unattended access.
    pub unattended_requires_stepup: bool,
    /// Master kill switch: when true every remote access request is refused.
    pub lockdown: bool,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            consent: ConsentMode::Attended,
            consent_timeout_secs: 30,
            unattended_requires_stepup: true,
            lockdown: false,
        }
    }
}

impl NexusConfig {
    /// Checks cross-field and value constraints that serde cannot express.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if let Some(url) = &self.net.signal_url {
            match url.scheme() {
                "wss" => {}
                // Plain `ws` is only acceptable for loopback development.
                "ws" if is_loopback_host(url) || self.net.allow_insecure_signal => {}
                other => {
                    return Err(ConfigError::Invalid {
                        key: "net.signal_url",
                        reason: format!(
                            "scheme `{other}` is not allowed (use wss://, or ws:// to localhost)"
                        ),
                    });
                }
            }
        }
        for url in &self.net.relay_urls {
            let ok = match url.scheme() {
                "https" => true,
                "http" => is_loopback_host(url),
                _ => false,
            };
            if !ok {
                return Err(ConfigError::Invalid {
                    key: "net.relay_urls",
                    reason: format!("`{url}` must be https:// (or http:// to localhost)"),
                });
            }
        }
        if self.security.consent_timeout_secs == 0 || self.security.consent_timeout_secs > 600 {
            return Err(ConfigError::Invalid {
                key: "security.consent_timeout_secs",
                reason: "must be between 1 and 600".into(),
            });
        }
        if self.terminal.max_sessions == 0 || self.terminal.detached_ttl_hours == 0 {
            return Err(ConfigError::Invalid {
                key: "terminal",
                reason: "max_sessions and detached_ttl_hours must be at least 1".into(),
            });
        }
        if self.files.max_upload_mib == 0 {
            return Err(ConfigError::Invalid {
                key: "files.max_upload_mib",
                reason: "must be at least 1".into(),
            });
        }
        // Reject unparsable filter directives early instead of silently logging nothing.
        tracing_subscriber::EnvFilter::try_new(&self.log.level).map_err(|e| {
            ConfigError::Invalid {
                key: "log.level",
                reason: e.to_string(),
            }
        })?;
        Ok(())
    }
}

fn is_loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Builder that layers configuration sources.
#[derive(Debug)]
pub struct ConfigLoader {
    figment: Figment,
}

impl Default for ConfigLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigLoader {
    /// Starts from compiled-in defaults.
    #[must_use]
    pub fn new() -> Self {
        Self {
            figment: Figment::from(Serialized::defaults(NexusConfig::default())),
        }
    }

    /// Merges a TOML file. A missing file is not an error; a malformed one is.
    #[must_use]
    pub fn with_file(mut self, path: impl AsRef<Path>) -> Self {
        self.figment = self.figment.merge(Toml::file(path));
        self
    }

    /// Merges a TOML document held in memory.
    #[must_use]
    pub fn with_toml_str(mut self, toml: &str) -> Self {
        self.figment = self.figment.merge(Toml::string(toml));
        self
    }

    /// Merges `NEXUS_*` environment variables (`__` separates nesting levels).
    #[must_use]
    pub fn with_env(mut self) -> Self {
        self.figment = self.figment.merge(Env::prefixed(ENV_PREFIX).split("__"));
        self
    }

    /// Extracts and validates the final configuration.
    pub fn load(self) -> Result<NexusConfig, ConfigError> {
        let cfg: NexusConfig = self.figment.extract()?;
        cfg.validate()?;
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use figment::Jail;

    use super::*;

    #[test]
    fn defaults_are_valid_and_conservative() {
        let cfg = ConfigLoader::new().load().unwrap();
        assert_eq!(cfg, NexusConfig::default());
        assert_eq!(cfg.security.consent, ConsentMode::Attended);
        assert!(!cfg.security.lockdown);
        assert!(cfg.security.unattended_requires_stepup);
    }

    #[test]
    fn toml_overrides_defaults() {
        let cfg = ConfigLoader::new()
            .with_toml_str(
                r#"
                [log]
                level = "debug"
                format = "json"
                [net]
                udp_port = 7842
                signal_url = "wss://signal.example.com/v1/ws"
                "#,
            )
            .load()
            .unwrap();
        assert_eq!(cfg.log.level, "debug");
        assert_eq!(cfg.log.format, LogFormat::Json);
        assert_eq!(cfg.net.udp_port, 7842);
        assert_eq!(
            cfg.net.signal_url.unwrap().as_str(),
            "wss://signal.example.com/v1/ws"
        );
    }

    #[test]
    #[allow(clippy::result_large_err)] // figment's `Jail` API dictates this closure signature
    fn env_overrides_file() {
        Jail::expect_with(|jail| {
            jail.create_file("config.toml", "[net]\nudp_port = 1111\n")?;
            jail.set_env("NEXUS_NET__UDP_PORT", "2222");
            jail.set_env("NEXUS_SECURITY__LOCKDOWN", "true");
            let cfg = ConfigLoader::new()
                .with_file("config.toml")
                .with_env()
                .load()
                .map_err(|e| e.to_string())?;
            assert_eq!(cfg.net.udp_port, 2222);
            assert!(cfg.security.lockdown);
            Ok(())
        });
    }

    #[test]
    fn missing_file_is_ignored() {
        let cfg = ConfigLoader::new()
            .with_file("/definitely/not/here.toml")
            .load()
            .unwrap();
        assert_eq!(cfg, NexusConfig::default());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let err = ConfigLoader::new()
            .with_toml_str("[security]\nlockdwon = true\n")
            .load()
            .unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)), "{err}");
    }

    #[test]
    fn insecure_signal_scheme_is_rejected() {
        let err = ConfigLoader::new()
            .with_toml_str("[net]\nsignal_url = \"ws://signal.example.com/v1/ws\"\n")
            .load()
            .unwrap_err();
        assert!(
            matches!(
                err,
                ConfigError::Invalid {
                    key: "net.signal_url",
                    ..
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn plaintext_signal_to_a_remote_host_needs_an_explicit_opt_in() {
        let url = "[net]\nsignal_url = \"ws://192.168.1.10:8443/v1/ws\"\n";
        assert!(ConfigLoader::new().with_toml_str(url).load().is_err());
        let opted_in = format!("{url}allow_insecure_signal = true\n");
        ConfigLoader::new().with_toml_str(&opted_in).load().unwrap();
        // The opt-in never relaxes the *relay* rule.
        let relay =
            "[net]\nallow_insecure_signal = true\nrelay_urls = [\"http://relay.example.com\"]\n";
        assert!(ConfigLoader::new().with_toml_str(relay).load().is_err());
    }

    #[test]
    fn plaintext_signal_is_allowed_for_loopback_only() {
        for ok in [
            "ws://localhost:8080/v1/ws",
            "ws://127.0.0.1:8080/v1/ws",
            "ws://[::1]:8080/x",
        ] {
            ConfigLoader::new()
                .with_toml_str(&format!("[net]\nsignal_url = \"{ok}\"\n"))
                .load()
                .unwrap();
        }
    }

    #[test]
    fn relay_must_be_https() {
        let err = ConfigLoader::new()
            .with_toml_str("[net]\nrelay_urls = [\"http://relay.example.com\"]\n")
            .load()
            .unwrap_err();
        assert!(
            matches!(
                err,
                ConfigError::Invalid {
                    key: "net.relay_urls",
                    ..
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn consent_timeout_bounds() {
        for bad in [0, 601] {
            let err = ConfigLoader::new()
                .with_toml_str(&format!("[security]\nconsent_timeout_secs = {bad}\n"))
                .load()
                .unwrap_err();
            assert!(matches!(err, ConfigError::Invalid { .. }));
        }
    }

    #[test]
    fn bad_log_filter_is_rejected() {
        let err = ConfigLoader::new()
            .with_toml_str("[log]\nlevel = \"nexus=notalevel\"\n")
            .load()
            .unwrap_err();
        assert!(
            matches!(
                err,
                ConfigError::Invalid {
                    key: "log.level",
                    ..
                }
            ),
            "{err}"
        );
    }
}
