//! Client for `nexus-signal`.
//!
//! [`connect`] opens a WebSocket (TLS via the OS trust store for `wss://`), proves possession of
//! the device key with an audience-bound, nonce-bound signature, and returns a cloneable
//! [`SignalClient`] for requests plus an [`EventStream`] of server pushes.

#![forbid(unsafe_code)]

mod error;
mod io;
mod tls;

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use nexus_crypto::{DeviceId, DeviceIdentity, SignContext};
use nexus_proto::signal::{CLIENT_NONCE_LEN, MAX_SIGNAL_MESSAGE, auth_message};
use nexus_proto::v1::{
    ClientAuth, DevicePresence, GoingOffline, GoingOfflineReason, Heartbeat, OsInfo, PresenceInfo,
    PublishPresence, RegisterPairing, RendezvousJoin, RendezvousLeave, RendezvousSend,
    RevokePairing, SendEnvelope, SignalClientMessage, SignalServerMessage, SignedPairingRecord,
    SignedRevocation, SubscribePresence, signal_client_message::Body as C,
    signal_server_message::Body as S,
};
use nexus_proto::version::PROTOCOL_VERSION;
use prost::Message as _;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tracing::debug;
use url::Url;

pub use error::ClientError;
pub use io::{EventStream, ServerEvent};

/// What the server told us at login.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerParams {
    /// How often to send a heartbeat.
    pub heartbeat_interval: Duration,
    /// How long the server waits before marking us offline.
    pub presence_ttl: Duration,
    /// Largest message the server accepts.
    pub max_message_bytes: usize,
}

/// Connection settings.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// `wss://host/v1/ws` (or `ws://localhost:port/v1/ws` for development).
    pub url: Url,
    /// Platform description sent at login (informational).
    pub os: OsInfo,
    /// Binary version string (informational).
    pub agent_version: String,
    /// Time allowed for TCP + TLS + WebSocket + login.
    pub connect_timeout: Duration,
    /// Time allowed per request.
    pub request_timeout: Duration,
}

impl ClientConfig {
    /// Defaults for `url`.
    #[must_use]
    pub fn new(url: Url) -> Self {
        Self {
            url,
            os: OsInfo::default(),
            agent_version: env!("CARGO_PKG_VERSION").to_owned(),
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(15),
        }
    }

    /// The audience this client signs: the URL's `host[:port]`. Must equal the server's
    /// configured audience.
    #[must_use]
    pub fn audience(&self) -> String {
        match (self.url.host_str(), self.url.port()) {
            (Some(host), Some(port)) => format!("{host}:{port}"),
            (Some(host), None) => host.to_owned(),
            (None, _) => String::new(),
        }
    }
}

/// A logged-in connection. Cheap to clone; all clones share one WebSocket.
#[derive(Debug, Clone)]
pub struct SignalClient {
    commands: mpsc::Sender<io::Command>,
    device_id: DeviceId,
    params: ServerParams,
    request_timeout: Duration,
}

/// Connects and logs in.
pub async fn connect(
    config: &ClientConfig,
    identity: Arc<DeviceIdentity>,
) -> Result<(SignalClient, EventStream), ClientError> {
    tokio::time::timeout(config.connect_timeout, connect_inner(config, identity))
        .await
        .map_err(|_| ClientError::Timeout)?
}

async fn connect_inner(
    config: &ClientConfig,
    identity: Arc<DeviceIdentity>,
) -> Result<(SignalClient, EventStream), ClientError> {
    let secure = match config.url.scheme() {
        "wss" => true,
        "ws" => false,
        _ => return Err(ClientError::BadUrl("scheme must be ws or wss")),
    };
    let connector = if secure {
        Some(tls::connector()?)
    } else {
        None
    };
    let ws_config = WebSocketConfig::default()
        .max_message_size(Some(MAX_SIGNAL_MESSAGE))
        .max_frame_size(Some(MAX_SIGNAL_MESSAGE));
    let (mut ws, _) = tokio_tungstenite::connect_async_tls_with_config(
        config.url.as_str(),
        Some(ws_config),
        false,
        connector,
    )
    .await
    .map_err(|e| ClientError::Connect(e.to_string()))?;

    // 1. The server speaks first: its nonce and the audience it believes it has.
    let hello = loop {
        match ws.next().await {
            Some(Ok(WsMessage::Binary(b))) => break SignalServerMessage::decode(b.as_ref()),
            Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_))) => {}
            _ => {
                return Err(ClientError::Protocol(
                    "connection closed before server hello",
                ));
            }
        }
    }
    .map_err(|_| ClientError::Protocol("malformed server hello"))?;
    let hello = match hello.body {
        Some(S::Hello(h)) => h,
        Some(S::Error(e)) => return Err(ClientError::from_server(&e)),
        _ => return Err(ClientError::Protocol("expected server hello")),
    };
    let server_major = hello.version.as_ref().map(|v| v.major);
    if server_major != Some(PROTOCOL_VERSION.major) {
        return Err(ClientError::Protocol(
            "incompatible server protocol version",
        ));
    }
    let audience = config.audience();
    if hello.audience != audience {
        return Err(ClientError::AudienceMismatch {
            expected: audience,
            server: hello.audience,
        });
    }

    // 2. Prove possession of the device key, bound to this server and these two nonces.
    let mut client_nonce = [0u8; CLIENT_NONCE_LEN];
    getrandom_fill(&mut client_nonce)?;
    let signature = identity.sign(
        SignContext::SignalAuth,
        &auth_message(&audience, &hello.nonce, &client_nonce),
    );
    let login_id = 1;
    let auth = SignalClientMessage {
        id: login_id,
        body: Some(C::Auth(ClientAuth {
            version: Some(PROTOCOL_VERSION),
            identity: identity.public().to_bytes().to_vec().into(),
            client_nonce: client_nonce.to_vec().into(),
            signature: signature.to_bytes().to_vec().into(),
            os: Some(config.os.clone()),
            agent_version: config.agent_version.clone(),
        })),
    };
    ws.send(WsMessage::Binary(auth.encode_to_vec().into()))
        .await
        .map_err(|e| ClientError::Connect(e.to_string()))?;

    let ok = loop {
        match ws.next().await {
            Some(Ok(WsMessage::Binary(b))) => {
                break SignalServerMessage::decode(b.as_ref())
                    .map_err(|_| ClientError::Protocol("malformed login reply"))?;
            }
            Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_))) => {}
            _ => return Err(ClientError::Protocol("connection closed during login")),
        }
    };
    let params = match ok.body {
        Some(S::AuthOk(a)) => ServerParams {
            heartbeat_interval: Duration::from_millis(u64::from(a.heartbeat_interval_ms)),
            presence_ttl: Duration::from_millis(u64::from(a.presence_ttl_ms)),
            max_message_bytes: usize::try_from(a.max_message_bytes).unwrap_or(MAX_SIGNAL_MESSAGE),
        },
        Some(S::Error(e)) => return Err(ClientError::from_server(&e)),
        _ => return Err(ClientError::Protocol("expected login confirmation")),
    };
    debug!("signaling login complete");

    let (commands, events) = io::spawn(ws);
    Ok((
        SignalClient {
            commands,
            device_id: identity.device_id(),
            params,
            request_timeout: config.request_timeout,
        },
        events,
    ))
}

fn getrandom_fill(buf: &mut [u8]) -> Result<(), ClientError> {
    getrandom::fill(buf).map_err(|_| ClientError::Protocol("system random number generator failed"))
}

impl SignalClient {
    /// This device's ID as the server knows it.
    #[must_use]
    pub fn device_id(&self) -> DeviceId {
        self.device_id
    }

    /// Parameters negotiated at login.
    #[must_use]
    pub fn params(&self) -> ServerParams {
        self.params
    }

    async fn request(&self, body: C) -> Result<S, ClientError> {
        let (reply, wait) = oneshot::channel();
        self.commands
            .send(io::Command::Request {
                body: Box::new(body),
                reply,
            })
            .await
            .map_err(|_| ClientError::Closed)?;
        match tokio::time::timeout(self.request_timeout, wait).await {
            Err(_) => Err(ClientError::Timeout),
            Ok(Err(_)) => Err(ClientError::Closed),
            Ok(Ok(result)) => result,
        }
    }

    async fn request_ack(&self, body: C) -> Result<(), ClientError> {
        match self.request(body).await? {
            S::Ack(_) => Ok(()),
            _ => Err(ClientError::Protocol("unexpected reply")),
        }
    }

    /// Publishes (or replaces) this device's presence.
    pub async fn publish_presence(&self, info: PresenceInfo) -> Result<(), ClientError> {
        self.request_ack(C::PublishPresence(PublishPresence { info: Some(info) }))
            .await
    }

    /// Refreshes the presence TTL. Fails with `NotFound` if presence must be republished.
    pub async fn heartbeat(&self, seq: u64) -> Result<(), ClientError> {
        self.request_ack(C::Heartbeat(Heartbeat { seq })).await
    }

    /// Tells peers why this device is about to go away (call from the OS power hook).
    pub async fn going_offline(&self, reason: GoingOfflineReason) -> Result<(), ClientError> {
        self.request_ack(C::GoingOffline(GoingOffline {
            reason: reason as i32,
        }))
        .await
    }

    /// Subscribes to presence of all paired devices; returns their current state.
    pub async fn subscribe_presence(&self) -> Result<Vec<DevicePresence>, ClientError> {
        match self
            .request(C::SubscribePresence(SubscribePresence {}))
            .await?
        {
            S::PresenceSnapshot(s) => Ok(s.devices),
            _ => Err(ClientError::Protocol("unexpected reply")),
        }
    }

    /// Registers a dual-signed pairing record.
    pub async fn register_pairing(&self, signed: SignedPairingRecord) -> Result<(), ClientError> {
        self.request_ack(C::RegisterPairing(RegisterPairing {
            signed: Some(signed),
        }))
        .await
    }

    /// Submits this device's revocation of a pairing.
    pub async fn revoke_pairing(&self, signed: SignedRevocation) -> Result<(), ClientError> {
        self.request_ack(C::RevokePairing(RevokePairing {
            signed: Some(signed),
        }))
        .await
    }

    /// All active pairings involving this device.
    pub async fn list_pairings(&self) -> Result<Vec<SignedPairingRecord>, ClientError> {
        match self
            .request(C::ListPairings(nexus_proto::v1::ListPairings {}))
            .await?
        {
            S::PairingList(l) => Ok(l.pairings),
            _ => Err(ClientError::Protocol("unexpected reply")),
        }
    }

    /// Sends a sealed envelope to a paired device.
    pub async fn send_envelope(&self, to: DeviceId, sealed: Vec<u8>) -> Result<(), ClientError> {
        self.request_ack(C::SendEnvelope(SendEnvelope {
            to: to.as_bytes().to_vec().into(),
            sealed: sealed.into(),
        }))
        .await
    }

    /// Joins the rendezvous room for a pairing code.
    pub async fn rendezvous_join(&self, room_id: [u8; 16]) -> Result<(), ClientError> {
        self.request_ack(C::RendezvousJoin(RendezvousJoin {
            room_id: room_id.to_vec().into(),
        }))
        .await
    }

    /// Sends bytes to the other device in the rendezvous room.
    pub async fn rendezvous_send(&self, data: Vec<u8>) -> Result<(), ClientError> {
        self.request_ack(C::RendezvousSend(RendezvousSend { data: data.into() }))
            .await
    }

    /// Leaves the rendezvous room.
    pub async fn rendezvous_leave(&self) -> Result<(), ClientError> {
        self.request_ack(C::RendezvousLeave(RendezvousLeave {}))
            .await
    }

    /// Closes the connection.
    pub async fn close(&self) {
        let _ = self.commands.send(io::Command::Close).await;
    }
}
