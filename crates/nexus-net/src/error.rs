//! Transport errors.

use nexus_proto::framing::FrameError;
use nexus_proto::version::NegotiationError;

/// Everything that can go wrong on a session.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    /// The local endpoint could not be created.
    #[error("could not start the network endpoint: {0}")]
    Bind(String),
    /// Dialling the peer failed (unreachable, handshake refused, …).
    #[error("could not connect: {0}")]
    Connect(String),
    /// The peer is not in the paired allow-list.
    #[error("peer is not paired with this device")]
    NotPaired,
    /// The connection ended.
    #[error("connection closed: {0}")]
    Closed(String),
    /// The peer violated the protocol.
    #[error("protocol violation: {0}")]
    Protocol(&'static str),
    /// A stream failed.
    #[error("stream error: {0}")]
    Stream(String),
    /// An operation exceeded its deadline.
    #[error("timed out")]
    Timeout,
    /// A control message was malformed or too large.
    #[error(transparent)]
    Frame(#[from] FrameError),
    /// The peers cannot agree on a protocol version.
    #[error(transparent)]
    Negotiate(#[from] NegotiationError),
}

impl NetError {
    pub(crate) fn stream(e: impl std::fmt::Display) -> Self {
        Self::Stream(e.to_string())
    }
}
