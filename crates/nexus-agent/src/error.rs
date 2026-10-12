//! Agent-level errors and their mapping onto IPC error codes.

use nexus_proto::v1::{IpcError, IpcErrorCode};

/// Why an agent operation failed.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// Target requires a user factor before accessing its resources.
    #[error("{0}")]
    StepUpRequired(String),
    /// The request is malformed.
    #[error("{0}")]
    Invalid(String),
    /// No such device / pairing.
    #[error("{0}")]
    NotFound(String),
    /// Another operation of the same kind is already running.
    #[error("{0}")]
    Busy(String),
    /// A needed service is not available right now (e.g. signaling is offline).
    #[error("{0}")]
    Unavailable(String),
    /// The operation was refused by policy.
    #[error("{0}")]
    Denied(String),
    /// Anything else.
    #[error("{0}")]
    Failed(String),
}

impl AgentError {
    pub(crate) fn code(&self) -> IpcErrorCode {
        match self {
            Self::StepUpRequired(_) => IpcErrorCode::StepUpRequired,
            Self::Invalid(_) => IpcErrorCode::InvalidRequest,
            Self::NotFound(_) => IpcErrorCode::NotFound,
            Self::Busy(_) => IpcErrorCode::Busy,
            Self::Unavailable(_) => IpcErrorCode::Unavailable,
            Self::Denied(_) => IpcErrorCode::PermissionDenied,
            Self::Failed(_) => IpcErrorCode::Failed,
        }
    }

    pub(crate) fn to_ipc(&self) -> IpcError {
        IpcError {
            code: self.code() as i32,
            message: self.to_string(),
        }
    }
}

impl From<nexus_net::NetError> for AgentError {
    fn from(e: nexus_net::NetError) -> Self {
        match e {
            nexus_net::NetError::NotPaired => Self::Denied("that device is not paired".into()),
            other => Self::Failed(other.to_string()),
        }
    }
}

impl From<nexus_trust::TrustError> for AgentError {
    fn from(e: nexus_trust::TrustError) -> Self {
        Self::Failed(e.to_string())
    }
}

impl From<nexus_signal_client::ClientError> for AgentError {
    fn from(e: nexus_signal_client::ClientError) -> Self {
        match e {
            nexus_signal_client::ClientError::Closed => {
                Self::Unavailable("signaling server disconnected".into())
            }
            other => Self::Failed(other.to_string()),
        }
    }
}
