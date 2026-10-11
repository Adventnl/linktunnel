//! The message channel a terminal runs on.

use std::future::Future;

use nexus_proto::v1::TerminalMessage;
use tokio::sync::mpsc;

use crate::TermError;

/// A bidirectional stream of [`TerminalMessage`]s.
pub trait TermIo: Send {
    /// Sends a message.
    fn send(&mut self, msg: TerminalMessage) -> impl Future<Output = Result<(), TermError>> + Send;
    /// Next message; `Ok(None)` when the peer closed.
    fn recv(&mut self) -> impl Future<Output = Result<Option<TerminalMessage>, TermError>> + Send;
}

/// An in-memory channel end for tests.
#[derive(Debug)]
pub struct MemoryTermIo {
    tx: mpsc::Sender<TerminalMessage>,
    rx: mpsc::Receiver<TerminalMessage>,
}

/// A connected pair.
#[must_use]
pub fn memory_pair() -> (MemoryTermIo, MemoryTermIo) {
    let (a_tx, b_rx) = mpsc::channel(64);
    let (b_tx, a_rx) = mpsc::channel(64);
    (
        MemoryTermIo { tx: a_tx, rx: a_rx },
        MemoryTermIo { tx: b_tx, rx: b_rx },
    )
}

impl TermIo for MemoryTermIo {
    async fn send(&mut self, msg: TerminalMessage) -> Result<(), TermError> {
        self.tx
            .send(msg)
            .await
            .map_err(|_| TermError::Stream("peer went away".into()))
    }

    async fn recv(&mut self) -> Result<Option<TerminalMessage>, TermError> {
        Ok(self.rx.recv().await)
    }
}
