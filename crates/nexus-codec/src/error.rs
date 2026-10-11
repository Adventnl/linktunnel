/// Codec failures.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    /// The configuration or an input frame is invalid (odd/zero size, wrong buffer length, …).
    #[error("invalid input: {0}")]
    Invalid(String),
    /// The backend failed to initialise.
    #[error("codec backend unavailable: {0}")]
    Unavailable(String),
    /// The encoder failed on a frame.
    #[error("encode failed: {0}")]
    Encode(String),
    /// The bitstream could not be decoded (corrupt, missing reference, …). The caller should ask
    /// the sender for a keyframe.
    #[error("decode failed: {0}")]
    Decode(String),
}
