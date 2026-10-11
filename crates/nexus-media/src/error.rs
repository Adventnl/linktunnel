/// Everything that can end or degrade a remote-desktop session.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    /// The transport failed.
    #[error(transparent)]
    Net(#[from] nexus_net::NetError),
    /// Encoding or decoding failed.
    #[error(transparent)]
    Codec(#[from] nexus_codec::CodecError),
    /// Screen capture failed.
    #[error(transparent)]
    Capture(#[from] nexus_capture::CaptureError),
    /// Input injection failed.
    #[error(transparent)]
    Input(#[from] nexus_input::InputError),
    /// The peer asked for something its grants do not allow.
    #[error("not permitted: {0}")]
    Denied(String),
    /// The peer sent something that violates the protocol.
    #[error("protocol violation: {0}")]
    Protocol(String),
    /// The session ended.
    #[error("session closed: {0}")]
    Closed(String),
}
