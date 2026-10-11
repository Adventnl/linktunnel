//! Carries terminal messages over a QUIC stream.

use nexus_net::{FramedStream, Stream};
use nexus_proto::framing::MAX_BULK_FRAME;
use nexus_proto::v1::TerminalMessage;
use nexus_terminal::{TermError, TermIo};

/// A `Terminal` stream as a [`TermIo`]. Frames may be large: re-attaching replays up to 1 MiB of
/// scrollback in one message.
#[derive(Debug)]
pub struct QuicTerm(FramedStream);

impl QuicTerm {
    /// Wraps a freshly opened or accepted terminal stream.
    #[must_use]
    pub fn new(stream: Stream) -> Self {
        Self(FramedStream::new(stream, MAX_BULK_FRAME))
    }
}

impl TermIo for QuicTerm {
    async fn send(&mut self, msg: TerminalMessage) -> Result<(), TermError> {
        self.0
            .send(&msg)
            .await
            .map_err(|e| TermError::Stream(e.to_string()))
    }

    async fn recv(&mut self) -> Result<Option<TerminalMessage>, TermError> {
        self.0
            .recv()
            .await
            .map_err(|e| TermError::Stream(e.to_string()))
    }
}
