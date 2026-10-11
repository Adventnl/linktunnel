use std::time::Duration;

use nexus_proto::v1::{SignalError, SignalErrorCode};

/// Why a signaling operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClientError {
    /// The URL is not usable.
    #[error("invalid signaling URL: {0}")]
    BadUrl(&'static str),
    /// TCP/TLS/WebSocket connection failed.
    #[error("could not connect: {0}")]
    Connect(String),
    /// TLS configuration failed (no usable trust store, …).
    #[error("TLS setup failed: {0}")]
    Tls(String),
    /// The server and client disagree about the server's name; the login proof would be useless.
    #[error("server identifies as `{server}` but this client expected `{expected}`")]
    AudienceMismatch {
        /// What this client signs.
        expected: String,
        /// What the server claims.
        server: String,
    },
    /// The peer broke the protocol.
    #[error("protocol error: {0}")]
    Protocol(&'static str),
    /// The server refused the request.
    #[error("server error {code:?}: {message}")]
    Server {
        /// Machine-readable reason.
        code: SignalErrorCode,
        /// Diagnostic text.
        message: String,
        /// If rate-limited: how long to wait.
        retry_after: Duration,
    },
    /// No reply in time.
    #[error("timed out")]
    Timeout,
    /// The connection is closed.
    #[error("connection closed")]
    Closed,
}

impl ClientError {
    pub(crate) fn from_server(e: &SignalError) -> Self {
        Self::Server {
            code: SignalErrorCode::try_from(e.code).unwrap_or(SignalErrorCode::Unspecified),
            message: e.message.clone(),
            retry_after: Duration::from_millis(u64::from(e.retry_after_ms)),
        }
    }

    /// The server's error code, if this is a server refusal.
    #[must_use]
    pub fn server_code(&self) -> Option<SignalErrorCode> {
        match self {
            Self::Server { code, .. } => Some(*code),
            _ => None,
        }
    }
}
