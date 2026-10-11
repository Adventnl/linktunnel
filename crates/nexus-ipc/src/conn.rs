use bytes::BytesMut;
use nexus_proto::framing::{FrameDecoder, encode_frame};
use nexus_proto::v1::IpcFrame;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf};

use crate::IpcError;

/// Largest IPC frame. Device lists are tiny; the limit exists for encoded video keyframes.
pub const MAX_IPC_FRAME: usize = 16 << 20;

/// Any byte stream the IPC can run over.
pub(crate) trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncStream for T {}

pub(crate) type BoxedStream = Box<dyn AsyncStream>;

/// The receiving half of an IPC connection.
pub struct IpcReader {
    read: ReadHalf<BoxedStream>,
    decoder: FrameDecoder,
    chunk: Vec<u8>,
}

/// The sending half of an IPC connection.
pub struct IpcWriter {
    write: WriteHalf<BoxedStream>,
}

/// An authenticated IPC connection.
pub struct IpcConnection {
    pub(crate) reader: IpcReader,
    pub(crate) writer: IpcWriter,
}

impl std::fmt::Debug for IpcConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("IpcConnection")
    }
}

impl IpcConnection {
    pub(crate) fn new(stream: BoxedStream) -> Self {
        let (read, write) = tokio::io::split(stream);
        Self {
            reader: IpcReader {
                read,
                decoder: FrameDecoder::new(MAX_IPC_FRAME),
                chunk: vec![0u8; 16 * 1024],
            },
            writer: IpcWriter { write },
        }
    }

    /// Splits into independently usable halves.
    #[must_use]
    pub fn split(self) -> (IpcReader, IpcWriter) {
        (self.reader, self.writer)
    }

    /// Receives the next frame; `None` on clean EOF.
    pub async fn recv(&mut self) -> Result<Option<IpcFrame>, IpcError> {
        self.reader.recv().await
    }

    /// Sends a frame.
    pub async fn send(&mut self, frame: &IpcFrame) -> Result<(), IpcError> {
        self.writer.send(frame).await
    }
}

impl IpcReader {
    /// Receives the next frame; `None` on clean EOF.
    pub async fn recv(&mut self) -> Result<Option<IpcFrame>, IpcError> {
        loop {
            if let Some(frame) = self.decoder.next_message::<IpcFrame>()? {
                return Ok(Some(frame));
            }
            let n = self.read.read(&mut self.chunk).await?;
            if n == 0 {
                return if self.decoder.buffered() == 0 {
                    Ok(None)
                } else {
                    Err(IpcError::Protocol("connection ended inside a frame"))
                };
            }
            self.decoder.extend(&self.chunk[..n]);
        }
    }
}

impl IpcWriter {
    /// Sends a frame.
    pub async fn send(&mut self, frame: &IpcFrame) -> Result<(), IpcError> {
        let mut buf = BytesMut::new();
        encode_frame(frame, &mut buf, MAX_IPC_FRAME)?;
        self.write.write_all(&buf).await?;
        self.write.flush().await?;
        Ok(())
    }
}
