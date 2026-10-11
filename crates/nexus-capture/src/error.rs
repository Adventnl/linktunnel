/// Capture failures.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    /// No display / capture service is reachable (headless box without a virtual display, a
    /// missing permission, …).
    #[error("no display available: {0}")]
    NoDisplay(String),
    /// A permission is missing (macOS Screen Recording, a Wayland portal denial).
    #[error("permission needed: {0}")]
    Permission(String),
    /// The requested monitor does not exist (it may have been unplugged).
    #[error("no such monitor: {0}")]
    NoSuchMonitor(u8),
    /// The platform cannot do this.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// The backend failed while capturing.
    #[error("capture failed: {0}")]
    Failed(String),
}
