/// Everything that can go wrong while capturing, coding or playing audio.
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    /// libopus rejected a call.
    #[error("opus: {0}")]
    Opus(String),
    /// The requested stream shape is not supported.
    #[error("unsupported audio configuration: {0}")]
    Config(String),
    /// The OS audio device failed or is missing.
    #[error("audio device: {0}")]
    Device(String),
    /// A datagram did not parse.
    #[error("malformed audio packet: {0}")]
    Packet(#[from] nexus_proto::wire::WireError),
    /// The source has no more audio (device unplugged, file finished).
    #[error("audio source ended")]
    Ended,
}

impl From<opus::Error> for AudioError {
    fn from(e: opus::Error) -> Self {
        Self::Opus(e.to_string())
    }
}
