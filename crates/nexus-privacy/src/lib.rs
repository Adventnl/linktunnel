//! Privacy is a scoped lease, never a permanent machine setting. Losing the session drops the
//! lease and restores local use. Unsupported platforms refuse the operation explicitly.
#![deny(unsafe_code)]
use std::sync::Arc;
/// Failure to establish physical-screen privacy.
#[derive(Debug, thiserror::Error)]
pub enum PrivacyError {
    /// OS/backend cannot preserve capture while obscuring the physical screen.
    #[error("privacy screen is unsupported by this platform/capture backend")]
    Unsupported,
    /// Another session already owns the screen.
    #[error("another session owns the privacy screen")]
    Busy,
    /// An OS call failed; privacy was not enabled.
    #[error("privacy screen failed: {0}")]
    Failed(String),
}
/// Active physical privacy. Dropping it restores local display and input.
pub trait PrivacyLease: Send {
    /// Renews the fail-safe watchdog. A failed renewal means privacy ended locally.
    fn renew(&self) -> Result<(), PrivacyError>;
}
/// An OS backend (or a recording backend in integration tests).
pub trait PrivacyBackend: Send + Sync {
    /// Enables all physical-monitor shields; optionally rejects physical local input.
    fn enable(&self, block_local_input: bool) -> Result<Box<dyn PrivacyLease>, PrivacyError>;
}
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows_impl;
/// Best supported backend. Windows normal desktop only; other platforms fail closed.
pub fn detect() -> Option<Arc<dyn PrivacyBackend>> {
    #[cfg(windows)]
    {
        Some(Arc::new(windows_impl::WindowsPrivacy))
    }
    #[cfg(not(windows))]
    {
        None
    }
}
