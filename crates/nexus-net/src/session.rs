//! An authenticated, encrypted connection to one paired peer.

use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use iroh::TransportAddr;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use nexus_proto::framing::MAX_CONTROL_FRAME;
use nexus_proto::v1::{Hello, PathKind, VideoCodec};
use nexus_proto::version::{Negotiated, negotiate};
use tokio::time::timeout;

use crate::NetError;
use crate::allow::CLOSE_NORMAL;
use crate::pathkind::classify_ip;
use crate::stream::{FramedStream, Stream, StreamKind};

/// How long the peers get to exchange `Hello`.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// Which end of the session this device is for capability negotiation (controller consumes
/// video, target produces it). Independent of who dialled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRole {
    /// Watches and controls.
    Controller,
    /// Is watched and controlled.
    Target,
}

/// Snapshot of the path currently carrying traffic.
#[derive(Debug, Clone, PartialEq)]
pub struct PathInfo {
    /// LAN, direct internet, or relay.
    pub kind: PathKind,
    /// Round-trip time estimate of the selected path.
    pub rtt: Duration,
    /// The peer's address on that path, for display.
    pub remote: String,
    /// How many paths the connection currently has open (e.g. relay + direct while probing).
    pub open_paths: usize,
}

/// The outcome of the `Hello` exchange.
#[derive(Debug)]
pub struct Established {
    /// The control stream, positioned after both hellos.
    pub control: FramedStream,
    /// What the peer announced.
    pub peer_hello: Hello,
    /// What both sides agreed on.
    pub negotiated: Negotiated,
}

/// A live connection to a paired device.
#[derive(Debug, Clone)]
pub struct Session {
    conn: Connection,
    peer: [u8; 32],
    dialed: bool,
}

impl Session {
    pub(crate) fn new(conn: Connection, dialed: bool) -> Self {
        let peer = *conn.remote_id().as_bytes();
        Self { conn, peer, dialed }
    }

    /// The peer's Ed25519 public key (authenticated by the TLS handshake).
    #[must_use]
    pub fn peer(&self) -> [u8; 32] {
        self.peer
    }

    /// True if this device initiated the connection.
    #[must_use]
    pub fn dialed(&self) -> bool {
        self.dialed
    }

    /// The underlying connection (for tests and advanced use).
    #[must_use]
    pub fn raw(&self) -> &Connection {
        &self.conn
    }

    /// Describes the path currently in use.
    #[must_use]
    pub fn path(&self) -> PathInfo {
        let paths = self.conn.paths();
        let open_paths = paths.len();
        let chosen = paths
            .iter()
            .find(|p| p.is_selected())
            .or_else(|| paths.iter().next());
        match chosen {
            None => PathInfo {
                kind: PathKind::Unspecified,
                rtt: Duration::ZERO,
                remote: String::new(),
                open_paths,
            },
            Some(p) => {
                let (kind, remote) = match p.remote_addr() {
                    TransportAddr::Ip(addr) => (classify_ip(addr.ip()), addr.to_string()),
                    TransportAddr::Relay(url) => (PathKind::Relay, url.to_string()),
                    other => (PathKind::Unspecified, format!("{other:?}")),
                };
                PathInfo {
                    kind,
                    rtt: p.rtt(),
                    remote,
                    open_paths,
                }
            }
        }
    }

    /// Waits until a direct (non-relay) path is selected, or `limit` passes. Returns whether one
    /// was found. Hole punching starts when the connection does, so this normally completes in
    /// well under a second on friendly networks.
    pub async fn wait_for_direct_path(&self, limit: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            if matches!(self.path().kind, PathKind::Lan | PathKind::Direct) {
                return true;
            }
            if tokio::time::Instant::now() >= deadline || self.conn.close_reason().is_some() {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// Opens a bidirectional stream of `kind`.
    pub async fn open_stream(&self, kind: StreamKind) -> Result<Stream, NetError> {
        let (mut send, recv) = self.conn.open_bi().await.map_err(NetError::stream)?;
        // QUIC streams are created lazily: writing the kind byte makes the stream visible to
        // the peer and tells it how to route the stream.
        send.write_all(&[kind as u8])
            .await
            .map_err(NetError::stream)?;
        Ok(Stream { send, recv })
    }

    /// Accepts the next bidirectional stream and reads its kind.
    pub async fn accept_stream(&self) -> Result<(StreamKind, Stream), NetError> {
        let (send, mut recv) = self.conn.accept_bi().await.map_err(NetError::stream)?;
        let mut kind = [0u8; 1];
        recv.read_exact(&mut kind).await.map_err(NetError::stream)?;
        Ok((StreamKind::from_u8(kind[0])?, Stream { send, recv }))
    }

    /// Opens a unidirectional stream of `kind` (e.g. one per video frame).
    pub async fn open_uni(&self, kind: StreamKind) -> Result<SendStream, NetError> {
        let mut send = self.conn.open_uni().await.map_err(NetError::stream)?;
        send.write_all(&[kind as u8])
            .await
            .map_err(NetError::stream)?;
        Ok(send)
    }

    /// Accepts the next unidirectional stream and reads its kind.
    pub async fn accept_uni(&self) -> Result<(StreamKind, RecvStream), NetError> {
        let mut recv = self.conn.accept_uni().await.map_err(NetError::stream)?;
        let mut kind = [0u8; 1];
        recv.read_exact(&mut kind).await.map_err(NetError::stream)?;
        Ok((StreamKind::from_u8(kind[0])?, recv))
    }

    /// Sends an unreliable datagram (cursor updates, audio).
    pub fn send_datagram(&self, data: Bytes) -> Result<(), NetError> {
        self.conn.send_datagram(data).map_err(NetError::stream)
    }

    /// Receives the next datagram.
    pub async fn recv_datagram(&self) -> Result<Bytes, NetError> {
        self.conn.read_datagram().await.map_err(NetError::stream)
    }

    /// Largest datagram currently deliverable, if datagrams are supported.
    #[must_use]
    pub fn max_datagram_size(&self) -> Option<usize> {
        self.conn.max_datagram_size()
    }

    /// Closes the session.
    pub fn close(&self, code: u32, reason: &str) {
        self.conn.close(code.into(), reason.as_bytes());
    }

    /// Closes the session normally.
    pub fn close_normal(&self) {
        self.close(CLOSE_NORMAL, "bye");
    }

    /// Resolves with a description once the connection is closed.
    pub async fn closed(&self) -> String {
        self.conn.closed().await.to_string()
    }

    /// Like [`closed`](Self::closed), but also says whether the end was *deliberate*: the peer or
    /// this side closed it on purpose (a goodbye, a revocation, an unpaired key), as opposed to
    /// the link dying (timeout, reset, transport error). Only the latter is worth reconnecting
    /// after.
    pub async fn closed_detail(&self) -> (String, bool) {
        use iroh::endpoint::ConnectionError;
        let e = self.conn.closed().await;
        let deliberate = match &e {
            ConnectionError::ApplicationClosed(c) => {
                let code = u64::from(c.error_code);
                // Our own deliberate codes: normal goodbye, unpaired, revoked, lockdown.
                code == u64::from(CLOSE_NORMAL) || (0x4E00..0x4F00).contains(&code)
            }
            ConnectionError::LocallyClosed => true,
            _ => false,
        };
        (e.to_string(), deliberate)
    }

    /// True once the connection has ended.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.conn.close_reason().is_some()
    }

    /// Exchanges `Hello` messages on a new control stream and negotiates the session.
    ///
    /// The dialling side opens the stream and speaks first; the accepting side answers. The
    /// exchange happens inside the authenticated, encrypted connection, so capabilities and
    /// version floors cannot be tampered with in transit.
    pub async fn establish(
        &self,
        local: &Hello,
        role: SessionRole,
        codec_preference: &[VideoCodec],
    ) -> Result<Established, NetError> {
        timeout(
            HELLO_TIMEOUT,
            self.establish_inner(local, role, codec_preference),
        )
        .await
        .map_err(|_| NetError::Timeout)?
    }

    async fn establish_inner(
        &self,
        local: &Hello,
        role: SessionRole,
        codec_preference: &[VideoCodec],
    ) -> Result<Established, NetError> {
        let mut control = if self.dialed {
            let stream = self.open_stream(StreamKind::Control).await?;
            let mut framed = FramedStream::new(stream, MAX_CONTROL_FRAME);
            framed.send(&hello_message(local)).await?;
            framed
        } else {
            let (kind, stream) = self.accept_stream().await?;
            if kind != StreamKind::Control {
                return Err(NetError::Protocol(
                    "first stream must be the control stream",
                ));
            }
            FramedStream::new(stream, MAX_CONTROL_FRAME)
        };
        let peer_hello = match control.recv::<nexus_proto::v1::ControlMessage>().await? {
            Some(nexus_proto::v1::ControlMessage {
                body: Some(nexus_proto::v1::control_message::Body::Hello(h)),
                ..
            }) => h,
            Some(_) => return Err(NetError::Protocol("expected hello")),
            None => return Err(NetError::Closed("peer closed before hello".into())),
        };
        if !self.dialed {
            control.send(&hello_message(local)).await?;
        }
        let (controller, target) = match role {
            SessionRole::Controller => (local, &peer_hello),
            SessionRole::Target => (&peer_hello, local),
        };
        let negotiated = negotiate(controller, target, codec_preference)?;
        Ok(Established {
            control,
            peer_hello,
            negotiated,
        })
    }
}

fn hello_message(hello: &Hello) -> nexus_proto::v1::ControlMessage {
    nexus_proto::v1::ControlMessage {
        body: Some(nexus_proto::v1::control_message::Body::Hello(hello.clone())),
        ..Default::default()
    }
}

/// The address of a peer as far as we know it: its key plus any hints for reaching it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerAddr {
    /// The peer's Ed25519 public key.
    pub key: [u8; 32],
    /// Its home relay, if it publishes one.
    pub relay: Option<String>,
    /// Direct addresses to try.
    pub direct: Vec<SocketAddr>,
}

impl PeerAddr {
    /// A peer known only by key (reachable only if a relay or discovery can find it).
    #[must_use]
    pub fn from_key(key: [u8; 32]) -> Self {
        Self {
            key,
            relay: None,
            direct: Vec::new(),
        }
    }

    /// Adds a home relay hint.
    #[must_use]
    pub fn with_relay(mut self, url: impl Into<String>) -> Self {
        self.relay = Some(url.into());
        self
    }

    /// Adds a direct address hint.
    #[must_use]
    pub fn with_direct(mut self, addr: SocketAddr) -> Self {
        self.direct.push(addr);
        self
    }
}

#[cfg(test)]
mod tests {
    use prost::Message as _;

    use super::*;

    #[test]
    fn peer_addr_builder() {
        let a = PeerAddr::from_key([1; 32])
            .with_relay("https://relay.example.com")
            .with_direct("192.0.2.1:7842".parse().unwrap());
        assert_eq!(a.relay.as_deref(), Some("https://relay.example.com"));
        assert_eq!(a.direct.len(), 1);
    }

    #[test]
    fn hello_message_wraps_the_hello() {
        let h = Hello {
            agent_version: "x".into(),
            ..Default::default()
        };
        let m = hello_message(&h);
        assert!(m.encoded_len() > 0);
        assert!(matches!(
            m.body,
            Some(nexus_proto::v1::control_message::Body::Hello(_))
        ));
    }
}
