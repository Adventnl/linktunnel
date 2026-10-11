//! The target side: one stream = one forwarded connection.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, info};

use crate::policy::ForwardPolicy;
use crate::{MAGIC, Status};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Counts live forwards against the policy's limit.
#[derive(Debug, Default)]
pub struct Active(AtomicUsize);

struct Slot(Arc<Active>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.0.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn read_request<R: AsyncRead + Unpin>(io: &mut R) -> Option<(String, u16)> {
    let mut magic = [0u8; 3];
    io.read_exact(&mut magic).await.ok()?;
    if &magic != MAGIC {
        return None;
    }
    let len = usize::from(io.read_u8().await.ok()?);
    if len == 0 {
        return None;
    }
    let mut host = vec![0u8; len];
    io.read_exact(&mut host).await.ok()?;
    let port = io.read_u16().await.ok()?;
    Some((String::from_utf8(host).ok()?, port))
}

/// Serves one forward on `io` (any bidirectional byte stream): reads the request, applies
/// `policy`, connects, reports the outcome and then copies bytes until either side closes.
pub async fn serve_stream<S>(mut io: S, policy: &ForwardPolicy, active: &Arc<Active>)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let reply = |s: Status| async move { s as u8 };
    let request = tokio::time::timeout(REQUEST_TIMEOUT, read_request(&mut io)).await;
    let Ok(Some((host, port))) = request else {
        let _ = io.write_all(&[reply(Status::BadRequest).await]).await;
        return;
    };
    if !policy.permits_name(&host, port) {
        info!(%host, port, "port forward refused by policy");
        let _ = io.write_all(&[Status::Denied as u8]).await;
        return;
    }
    if active.0.fetch_add(1, Ordering::SeqCst) >= policy.max_connections {
        active.0.fetch_sub(1, Ordering::SeqCst);
        let _ = io.write_all(&[Status::Denied as u8]).await;
        return;
    }
    let _slot = Slot(Arc::clone(active));

    // Resolve ourselves so the *address* can be checked, not just the name.
    let addrs = match tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio::net::lookup_host((host.as_str(), port)),
    )
    .await
    {
        Ok(Ok(a)) => a.collect::<Vec<_>>(),
        _ => {
            let _ = io.write_all(&[Status::Unreachable as u8]).await;
            return;
        }
    };
    let Some(addr) = addrs
        .into_iter()
        .find(|a| policy.permits_addr(&host, port, *a))
    else {
        info!(%host, port, "port forward refused: no resolved address is allowed");
        let _ = io.write_all(&[Status::Denied as u8]).await;
        return;
    };
    let tcp = match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => {
            debug!(%addr, error = %e, "forward destination refused the connection");
            let status = if e.kind() == std::io::ErrorKind::ConnectionRefused {
                Status::Refused
            } else {
                Status::Unreachable
            };
            let _ = io.write_all(&[status as u8]).await;
            return;
        }
        Err(_) => {
            let _ = io.write_all(&[Status::Unreachable as u8]).await;
            return;
        }
    };
    let _ = tcp.set_nodelay(true);
    if io.write_all(&[Status::Ok as u8]).await.is_err() {
        return;
    }
    let mut tcp = tcp;
    let _ = tokio::io::copy_bidirectional(&mut io, &mut tcp).await;
    let _ = io.shutdown().await;
}

/// A target's forwarding service: the policy plus the live-connection count.
#[derive(Debug, Default)]
pub struct ForwardHost {
    policy: ForwardPolicy,
    active: Arc<Active>,
}

impl ForwardHost {
    /// A service applying `policy`.
    #[must_use]
    pub fn new(policy: ForwardPolicy) -> Arc<Self> {
        Arc::new(Self {
            policy,
            active: Arc::default(),
        })
    }

    /// Serves one forward stream.
    pub async fn serve<S>(&self, io: S)
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        serve_stream(io, &self.policy, &self.active).await;
    }
}
