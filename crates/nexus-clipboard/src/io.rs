//! The message channel clipboard sync runs on.

use std::future::Future;

use nexus_net::FramedStream;
use nexus_proto::v1::ClipboardUpdate;
use tokio::sync::mpsc;

use crate::error::ClipError;

/// A bidirectional stream of updates.
pub trait ClipIo: Send {
    /// Sends an update.
    fn send(
        &mut self,
        update: ClipboardUpdate,
    ) -> impl Future<Output = Result<(), ClipError>> + Send;
    /// Next update; `Ok(None)` when the peer closed.
    fn recv(&mut self) -> impl Future<Output = Result<Option<ClipboardUpdate>, ClipError>> + Send;
}

impl ClipIo for FramedStream {
    async fn send(&mut self, update: ClipboardUpdate) -> Result<(), ClipError> {
        Ok(FramedStream::send(self, &update).await?)
    }

    async fn recv(&mut self) -> Result<Option<ClipboardUpdate>, ClipError> {
        Ok(FramedStream::recv(self).await?)
    }
}

/// An in-memory channel end for tests.
#[derive(Debug)]
pub struct MemoryClipIo {
    tx: mpsc::Sender<ClipboardUpdate>,
    rx: mpsc::Receiver<ClipboardUpdate>,
}

/// A connected pair.
#[must_use]
pub fn memory_pair() -> (MemoryClipIo, MemoryClipIo) {
    let (a_tx, b_rx) = mpsc::channel(16);
    let (b_tx, a_rx) = mpsc::channel(16);
    (
        MemoryClipIo { tx: a_tx, rx: a_rx },
        MemoryClipIo { tx: b_tx, rx: b_rx },
    )
}

impl ClipIo for MemoryClipIo {
    async fn send(&mut self, update: ClipboardUpdate) -> Result<(), ClipError> {
        self.tx
            .send(update)
            .await
            .map_err(|_| ClipError::Stream("peer went away".into()))
    }

    async fn recv(&mut self) -> Result<Option<ClipboardUpdate>, ClipError> {
        Ok(self.rx.recv().await)
    }
}
