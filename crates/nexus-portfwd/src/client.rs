//! The controller side.

use std::future::Future;
use std::net::SocketAddr;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, warn};

use crate::{MAGIC, Status};

/// Why a forward could not be set up.
#[derive(Debug, thiserror::Error)]
pub enum ForwardError {
    /// The target's policy does not allow that destination.
    #[error("the device does not allow forwarding to that address")]
    Denied,
    /// Nothing listens there.
    #[error("the connection was refused by the destination")]
    Refused,
    /// The destination did not answer.
    #[error("the destination is unreachable from the device")]
    Unreachable,
    /// The host name is too long or the like.
    #[error("invalid forward request: {0}")]
    Invalid(&'static str),
    /// The stream failed.
    #[error("stream error: {0}")]
    Io(String),
}

/// Sends the request on a fresh stream and waits for the target's verdict. On success `io` is a
/// transparent byte pipe to `host:port` on the target.
pub async fn open_forward<S>(io: &mut S, host: &str, port: u16) -> Result<(), ForwardError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let host_bytes = host.as_bytes();
    let len = u8::try_from(host_bytes.len())
        .ok()
        .filter(|l| *l > 0)
        .ok_or(ForwardError::Invalid("host name must be 1–255 bytes"))?;
    let mut req = Vec::with_capacity(6 + host_bytes.len());
    req.extend_from_slice(MAGIC);
    req.push(len);
    req.extend_from_slice(host_bytes);
    req.extend_from_slice(&port.to_be_bytes());
    let io_err = |e: std::io::Error| ForwardError::Io(e.to_string());
    io.write_all(&req).await.map_err(io_err)?;
    io.flush().await.map_err(io_err)?;
    let status = io.read_u8().await.map_err(io_err)?;
    match Status::from_u8(status) {
        Status::Ok => Ok(()),
        Status::Denied => Err(ForwardError::Denied),
        Status::Refused => Err(ForwardError::Refused),
        Status::Unreachable => Err(ForwardError::Unreachable),
        Status::BadRequest => Err(ForwardError::Invalid("the device rejected the request")),
    }
}

/// Accepts connections on `listener` and forwards each one to `host:port` through a stream from
/// `open_stream`, until the listener fails or the returned future is dropped.
pub async fn listen_and_forward<S, F, Fut>(
    listener: TcpListener,
    host: String,
    port: u16,
    open_stream: F,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    F: Fn() -> Fut + Send + Sync + Clone + 'static,
    Fut: Future<Output = Result<S, String>> + Send,
{
    loop {
        let (tcp, peer): (TcpStream, SocketAddr) = match listener.accept().await {
            Ok(ok) => ok,
            Err(e) => {
                warn!(error = %e, "forward listener failed");
                return;
            }
        };
        let (host, open) = (host.clone(), open_stream.clone());
        tokio::spawn(async move {
            let _ = tcp.set_nodelay(true);
            let mut stream = match open().await {
                Ok(s) => s,
                Err(e) => {
                    debug!(%peer, error = %e, "could not open a forward stream");
                    return;
                }
            };
            if let Err(e) = open_forward(&mut stream, &host, port).await {
                debug!(%peer, error = %e, "forward refused");
                return;
            }
            let mut tcp = tcp;
            let _ = tokio::io::copy_bidirectional(&mut tcp, &mut stream).await;
        });
    }
}
