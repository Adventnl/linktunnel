//! The Windows service that makes remote control work at the sign-in screen and on UAC prompts.
//!
//! # Why a service, and why this shape
//! Windows runs services in session 0, which has no desktop to capture, and a normal user process
//! cannot see the *secure desktop* (UAC consent, lock screen, sign-in) at all. The only process
//! that can is SYSTEM, running in the user's session. So:
//!
//! ```text
//!   SCM ──starts──▶ nexus-agent.exe --windows-service        (SYSTEM, session 0: this crate)
//!                          │  watches WTS session changes, supervises one child
//!                          ▼
//!                   nexus-agent.exe --system ...            (SYSTEM token, *console session*)
//!                          │  capture + input threads follow the input desktop
//!                          ▼  (nexus-desktop: Default ⇄ Winlogon)
//!                   QUIC session ⇄ controller
//! ```
//!
//! The child is the whole agent (network, trust store, presence, media), not a thin helper:
//! a QUIC session cannot be handed between processes, and one agent is simpler to reason about
//! than two with a protocol between them. When the console session changes (fast user
//! switching, logoff, RDP takeover) the supervisor replaces the child; presence flaps for a
//! second and controllers reconnect.
//!
//! Attended-consent prompts cannot go through the usual IPC UI (the SYSTEM agent's IPC is
//! admin-only), so the child shows them itself with `WTSSendMessage` ([`prompt`]).
//!
//! The portable decision logic lives in [`supervisor`] and is unit-tested everywhere; the Win32
//! parts are compiled only on Windows and are **type-checked but not yet run** on a real machine
//! (docs/STATUS.md says so too).

#![cfg_attr(not(windows), forbid(unsafe_code))]

pub mod cmdline;
pub mod supervisor;

#[cfg(windows)]
pub mod acl;
#[cfg(windows)]
pub mod launcher;
#[cfg(windows)]
pub mod prompt;
#[cfg(windows)]
pub mod service;

/// Name the service is registered under.
pub const SERVICE_NAME: &str = "NexusAgent";
/// Name shown in the Services list.
pub const SERVICE_DISPLAY_NAME: &str = "Nexus Remote Agent";

/// Errors from the service machinery.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// A Win32 call failed.
    #[error("{what}: {detail}")]
    Os {
        /// What was being done.
        what: &'static str,
        /// The OS error.
        detail: String,
    },
    /// Not running on Windows.
    #[error("the Windows service only exists on Windows")]
    Unsupported,
}

/// Runs the Windows service dispatcher; `child_args` are appended to the child agent's command
/// line. Blocks until the service stops.
#[cfg(windows)]
pub fn run(child_args: Vec<String>) -> Result<(), ServiceError> {
    service::run(child_args)
}

/// Stub for other platforms.
#[cfg(not(windows))]
pub fn run(_child_args: Vec<String>) -> Result<(), ServiceError> {
    Err(ServiceError::Unsupported)
}
