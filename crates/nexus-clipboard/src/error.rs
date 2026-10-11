use nexus_net::NetError;

/// Why a clipboard operation failed.
#[derive(Debug, thiserror::Error)]
pub enum ClipError {
    /// The OS clipboard could not be read or written.
    #[error("clipboard unavailable: {0}")]
    Backend(String),
    /// A received value was malformed or too large.
    #[error("rejected clipboard content: {0}")]
    Rejected(String),
    /// The stream died.
    #[error("clipboard stream: {0}")]
    Stream(String),
}

impl From<NetError> for ClipError {
    fn from(e: NetError) -> Self {
        Self::Stream(e.to_string())
    }
}
