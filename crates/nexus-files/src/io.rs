//! The message transport the file protocol runs on, so that it can be tested without QUIC.

use std::future::Future;

use nexus_net::{FramedStream, Session, StreamKind};
use nexus_proto::framing::MAX_BULK_FRAME;
use nexus_proto::v1::FileMessage;
use tokio::sync::mpsc;

use crate::error::FileError;

/// One file operation's bidirectional message channel.
pub trait MsgIo: Send {
    /// Sends a message.
    fn send(&mut self, msg: FileMessage) -> impl Future<Output = Result<(), FileError>> + Send;
    /// Receives the next message; `Ok(None)` if the peer finished cleanly.
    fn recv(&mut self) -> impl Future<Output = Result<Option<FileMessage>, FileError>> + Send;
    /// Tells the peer nothing more will be sent.
    fn finish(&mut self) -> Result<(), FileError>;
}

impl MsgIo for FramedStream {
    async fn send(&mut self, msg: FileMessage) -> Result<(), FileError> {
        Ok(FramedStream::send(self, &msg).await?)
    }

    async fn recv(&mut self) -> Result<Option<FileMessage>, FileError> {
        Ok(FramedStream::recv(self).await?)
    }

    fn finish(&mut self) -> Result<(), FileError> {
        Ok(FramedStream::finish(self)?)
    }
}

/// Wraps a freshly accepted `Files` stream.
#[must_use]
pub fn framed(stream: nexus_net::Stream) -> FramedStream {
    FramedStream::new(stream, MAX_BULK_FRAME)
}

/// Opens one stream per operation.
pub trait Opener: Send + Sync {
    /// The channel type it yields.
    type Io: MsgIo;
    /// Opens a new operation channel.
    fn open(&self) -> impl Future<Output = Result<Self::Io, FileError>> + Send;
}

impl Opener for Session {
    type Io = FramedStream;

    async fn open(&self) -> Result<FramedStream, FileError> {
        let stream = self.open_stream(StreamKind::Files).await?;
        Ok(framed(stream))
    }
}

/// An in-memory channel end, for tests and for local (same-process) use.
#[derive(Debug)]
pub struct MemoryIo {
    tx: Option<mpsc::Sender<FileMessage>>,
    rx: mpsc::Receiver<FileMessage>,
}

/// A connected pair of in-memory channel ends.
#[must_use]
pub fn memory_pair() -> (MemoryIo, MemoryIo) {
    let (a_tx, b_rx) = mpsc::channel(8);
    let (b_tx, a_rx) = mpsc::channel(8);
    (
        MemoryIo {
            tx: Some(a_tx),
            rx: a_rx,
        },
        MemoryIo {
            tx: Some(b_tx),
            rx: b_rx,
        },
    )
}

impl MsgIo for MemoryIo {
    async fn send(&mut self, msg: FileMessage) -> Result<(), FileError> {
        let tx = self
            .tx
            .as_ref()
            .ok_or_else(|| FileError::Protocol("send after finish".into()))?;
        tx.send(msg)
            .await
            .map_err(|_| FileError::Protocol("peer went away".into()))
    }

    async fn recv(&mut self) -> Result<Option<FileMessage>, FileError> {
        Ok(self.rx.recv().await)
    }

    fn finish(&mut self) -> Result<(), FileError> {
        self.tx = None;
        Ok(())
    }
}
