//! Shared foundations for every Nexus Remote component.
//!
//! * [`config`]: layered configuration (defaults, file, `NEXUS_*` environment).
//! * [`paths`]: per-OS config/data/log locations for user and system scope.
//! * [`telemetry`]: `tracing` initialisation and latency histograms.
//! * [`clock`]: a monotonic microsecond clock used to timestamp media and input.
//! * [`i18n`]: the English / Simplified Chinese message catalogue.
//!
//! This crate deliberately has no networking, crypto or OS-specific dependencies so
//! that every other crate can depend on it.

#![forbid(unsafe_code)]

pub mod clock;
pub mod config;
pub mod i18n;
pub mod paths;
pub mod telemetry;

pub use config::{
    ClipboardConfig, ConfigError, FilesConfig, ForwardTarget, NexusConfig, PortForwardConfig,
    PowerConfig, SharedFolder, TerminalConfig,
};
pub use paths::{Paths, Scope};
