//! Shared test harness: a real server on a loopback port and real WebSocket clients.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use nexus_crypto::{DeviceIdentity, IdentitySeed, SignContext};
use nexus_proto::signal::auth_message;
use nexus_proto::v1::{
    ClientAuth, SignalClientMessage, SignalServerMessage, SignedPairingRecord,
    signal_client_message::Body as C, signal_server_message::Body as S,
};
use nexus_proto::version::PROTOCOL_VERSION;
use nexus_signal::store::{MemoryPairingStore, MemoryPresenceStore};
use nexus_signal::{Hub, SignalConfig, serve};
use nexus_signal_client::{ClientConfig, EventStream, ServerEvent, SignalClient};
use nexus_trust::{grants, record};
use prost::Message as _;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use url::Url;

pub struct TestServer {
    pub addr: SocketAddr,
    pub hub: Arc<Hub>,
    pub clock: Arc<AtomicU64>,
    _shutdown: oneshot::Sender<()>,
}

impl TestServer {
    pub async fn start() -> Self {
        Self::start_with(|_| {}).await
    }

    pub async fn start_with(tweak: impl FnOnce(&mut SignalConfig)) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut config = SignalConfig::new(addr.to_string());
        tweak(&mut config);
        let clock = Arc::new(AtomicU64::new(nexus_core::clock::unix_ms()));
        let c = Arc::clone(&clock);
        let hub = Hub::with_clock(
            config,
            Arc::new(MemoryPairingStore::new()),
            Arc::new(MemoryPresenceStore::new()),
            Arc::new(move || c.load(Ordering::SeqCst)),
        );
        let (tx, rx) = oneshot::channel::<()>();
        tokio::spawn(serve(listener, Arc::clone(&hub), async {
            let _ = rx.await;
        }));
        Self {
            addr,
            hub,
            clock,
            _shutdown: tx,
        }
    }

    pub fn url(&self) -> Url {
        format!("ws://{}/v1/ws", self.addr).parse().unwrap()
    }

    pub fn now(&self) -> u64 {
        self.clock.load(Ordering::SeqCst)
    }

    pub fn advance(&self, ms: u64) {
        self.clock.fetch_add(ms, Ordering::SeqCst);
    }

    pub async fn connect(&self, id: &Arc<DeviceIdentity>) -> (SignalClient, EventStream) {
        let mut cfg = ClientConfig::new(self.url());
        cfg.request_timeout = Duration::from_secs(5);
        nexus_signal_client::connect(&cfg, Arc::clone(id))
            .await
            .unwrap()
    }

    pub async fn raw(&self) -> RawClient {
        let (ws, _) = tokio_tungstenite::connect_async(self.url().as_str())
            .await
            .unwrap();
        RawClient { ws }
    }
}

pub fn device(byte: u8) -> Arc<DeviceIdentity> {
    Arc::new(DeviceIdentity::from_seed(&IdentitySeed::from_bytes(
        [byte; 32],
    )))
}

/// A correctly dual-signed record between `x` and `y`, created at `at`.
pub fn signed_pairing(x: &DeviceIdentity, y: &DeviceIdentity, at: u64) -> SignedPairingRecord {
    let g = grants::all();
    let bytes = record::build_record(x.public(), "X", y.public(), "Y", &g, &g, at).unwrap();
    record::assemble(
        &bytes,
        x.public(),
        &record::sign_record(x, &bytes),
        &record::sign_record(y, &bytes),
    )
    .unwrap()
}

/// Pairs `a` and `b` on the server by registering a valid record from `a`.
pub async fn pair(
    server: &TestServer,
    a: &SignalClient,
    a_id: &DeviceIdentity,
    b_id: &DeviceIdentity,
) -> SignedPairingRecord {
    let signed = signed_pairing(a_id, b_id, server.now());
    a.register_pairing(signed.clone()).await.unwrap();
    signed
}

pub async fn next_event(events: &mut EventStream) -> ServerEvent {
    tokio::time::timeout(Duration::from_secs(3), events.next())
        .await
        .expect("timed out waiting for a server event")
        .expect("event stream ended")
}

/// Asserts nothing arrives for a short while.
pub async fn no_event(events: &mut EventStream) {
    if let Ok(Some(ev)) = tokio::time::timeout(Duration::from_millis(300), events.next()).await {
        panic!("unexpected event: {ev:?}");
    }
}

pub struct RawClient {
    pub ws: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
}

impl RawClient {
    pub async fn recv(&mut self) -> Option<SignalServerMessage> {
        loop {
            match tokio::time::timeout(Duration::from_secs(3), self.ws.next()).await {
                Ok(Some(Ok(WsMessage::Binary(b)))) => {
                    return Some(SignalServerMessage::decode(b.as_ref()).unwrap());
                }
                Ok(Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_)))) => {}
                _ => return None,
            }
        }
    }

    pub async fn send(&mut self, msg: SignalClientMessage) {
        let _ = self
            .ws
            .send(WsMessage::Binary(msg.encode_to_vec().into()))
            .await;
    }

    /// Reads the server hello and returns (nonce, audience).
    pub async fn hello(&mut self) -> (Vec<u8>, String) {
        match self.recv().await.and_then(|m| m.body) {
            Some(S::Hello(h)) => (h.nonce.to_vec(), h.audience),
            other => panic!("expected hello, got {other:?}"),
        }
    }

    pub fn auth_message(
        identity: &DeviceIdentity,
        audience: &str,
        nonce: &[u8],
        client_nonce: &[u8],
    ) -> SignalClientMessage {
        let sig = identity.sign(
            SignContext::SignalAuth,
            &auth_message(audience, nonce, client_nonce),
        );
        SignalClientMessage {
            id: 1,
            body: Some(C::Auth(ClientAuth {
                version: Some(PROTOCOL_VERSION),
                identity: identity.public().to_bytes().to_vec().into(),
                client_nonce: client_nonce.to_vec().into(),
                signature: sig.to_bytes().to_vec().into(),
                ..Default::default()
            })),
        }
    }

    /// True when the server closes the connection (EOF/close/error) within a few seconds.
    pub async fn closed(&mut self) -> bool {
        loop {
            match tokio::time::timeout(Duration::from_secs(3), self.ws.next()).await {
                Ok(Some(Ok(WsMessage::Binary(_) | WsMessage::Ping(_) | WsMessage::Pong(_)))) => {}
                Ok(None | Some(Err(_) | Ok(WsMessage::Close(_)))) => return true,
                Ok(Some(Ok(_))) => {}
                Err(_) => return false,
            }
        }
    }
}

pub fn presence(name: &str, seq: u64) -> nexus_proto::v1::PresenceInfo {
    nexus_proto::v1::PresenceInfo {
        state: nexus_proto::v1::PresenceState::Online as i32,
        name: name.into(),
        seq,
        ..Default::default()
    }
}
