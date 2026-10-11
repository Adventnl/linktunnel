/// Injection failures.
#[derive(Debug, thiserror::Error)]
pub enum InputError {
    /// No display / input service is reachable.
    #[error("no input service available: {0}")]
    Unavailable(String),
    /// The platform cannot express this event (touch on X11, pen without uinput, …).
    #[error("unsupported input: {0}")]
    Unsupported(String),
    /// A permission is missing (macOS Accessibility).
    #[error("permission needed: {0}")]
    Permission(String),
    /// The event refers to something that does not exist (unknown monitor, unknown key).
    #[error("invalid input: {0}")]
    Invalid(String),
    /// The backend failed.
    #[error("injection failed: {0}")]
    Failed(String),
}
