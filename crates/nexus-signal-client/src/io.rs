//! The background task that owns the WebSocket.

use std::collections::HashMap;

use futures_util::{SinkExt, StreamExt};
use nexus_crypto::DeviceId;
use nexus_proto::v1::{
    DevicePresence, SignalClientMessage, SignalServerMessage, SignedPairingRecord,
    SignedRevocation, signal_client_message::Body as C, signal_server_message::Body as S,
};
use prost::Message as _;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tracing::debug;

use crate::error::ClientError;

pub(crate) type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub(crate) enum Command {
    Request {
        body: Box<C>,
        reply: oneshot::Sender<Result<S, ClientError>>,
    },
    Close,
}

/// Something the server pushed to us.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerEvent {
    /// A paired device's presence changed.
    PresenceUpdate(Box<DevicePresence>),
    /// A new pairing involving this device was registered.
    PairingAdded(SignedPairingRecord),
    /// The peer revoked its pairing with this device. Drop live sessions with it immediately.
    PairingRevoked {
        /// The device that revoked.
        peer: DeviceId,
        /// Its signed revocation (verify before acting on it).
        signed: SignedRevocation,
    },
    /// A sealed envelope from a paired device.
    Envelope {
        /// Sender.
        from: DeviceId,
        /// Noise-`X` blob to open with `nexus_crypto::noise::envelope`.
        sealed: Vec<u8>,
        /// Server-assigned sequence number.
        seq: u64,
    },
    /// The other device joined (`true`) or left (`false`) our rendezvous room.
    RendezvousPeer {
        /// Whether the peer is now present.
        joined: bool,
    },
    /// Bytes from the other device in our rendezvous room.
    RendezvousData(Vec<u8>),
    /// A newer login from the same device replaced this connection.
    Superseded,
    /// The connection ended.
    Disconnected(Option<ClientError>),
}

/// Receiver of [`ServerEvent`]s.
#[derive(Debug)]
pub struct EventStream {
    rx: mpsc::Receiver<ServerEvent>,
}

impl EventStream {
    /// Waits for the next event; `None` after [`ServerEvent::Disconnected`] has been delivered.
    pub async fn next(&mut self) -> Option<ServerEvent> {
        self.rx.recv().await
    }
}

pub(crate) fn spawn(socket: Socket) -> (mpsc::Sender<Command>, EventStream) {
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let (ev_tx, ev_rx) = mpsc::channel(256);
    tokio::spawn(run(socket, cmd_rx, ev_tx));
    (cmd_tx, EventStream { rx: ev_rx })
}

fn device_id(bytes: &[u8]) -> Option<DeviceId> {
    DeviceId::from_slice(bytes).ok()
}

async fn run(
    socket: Socket,
    mut commands: mpsc::Receiver<Command>,
    events: mpsc::Sender<ServerEvent>,
) {
    let (mut sink, mut stream) = socket.split();
    let mut pending: HashMap<u64, oneshot::Sender<Result<S, ClientError>>> = HashMap::new();
    // Id 1 was used by the login request.
    let mut next_id: u64 = 2;
    let mut end: Option<ClientError> = None;

    'outer: loop {
        tokio::select! {
            command = commands.recv() => match command {
                None => break 'outer,
                Some(Command::Close) => {
                    let _ = sink.close().await;
                    break 'outer;
                }
                Some(Command::Request { body, reply }) => {
                    let id = next_id;
                    next_id += 1;
                    let msg = SignalClientMessage { id, body: Some(*body) };
                    if sink.send(WsMessage::Binary(msg.encode_to_vec().into())).await.is_err() {
                        let _ = reply.send(Err(ClientError::Closed));
                        end = Some(ClientError::Closed);
                        break 'outer;
                    }
                    pending.insert(id, reply);
                }
            },
            incoming = stream.next() => match incoming {
                None | Some(Ok(WsMessage::Close(_))) => break 'outer,
                Some(Err(e)) => {
                    end = Some(ClientError::Connect(e.to_string()));
                    break 'outer;
                }
                Some(Ok(WsMessage::Binary(bytes))) => {
                    let Ok(msg) = SignalServerMessage::decode(bytes.as_ref()) else {
                        end = Some(ClientError::Protocol("malformed server message"));
                        break 'outer;
                    };
                    if msg.in_reply_to != 0 {
                        if let Some(reply) = pending.remove(&msg.in_reply_to) {
                            let result = match msg.body {
                                Some(S::Error(e)) => Err(ClientError::from_server(&e)),
                                Some(body) => Ok(body),
                                None => Err(ClientError::Protocol("empty reply")),
                            };
                            let _ = reply.send(result);
                        }
                        continue;
                    }
                    if let Some(event) = to_event(msg.body) {
                        let superseded = event == ServerEvent::Superseded;
                        // A consumer that stopped reading must not stall the connection.
                        let _ = events.try_send(event);
                        if superseded { break 'outer; }
                    }
                }
                Some(Ok(_)) => {} // pings/pongs are answered by tungstenite
            },
        }
    }

    for (_, reply) in pending.drain() {
        let _ = reply.send(Err(ClientError::Closed));
    }
    debug!("signaling connection ended");
    let _ = events.send(ServerEvent::Disconnected(end)).await;
}

fn to_event(body: Option<S>) -> Option<ServerEvent> {
    Some(match body? {
        S::PresenceUpdate(u) => ServerEvent::PresenceUpdate(Box::new(u.device?)),
        S::PairingAdded(a) => ServerEvent::PairingAdded(a.signed?),
        S::PairingRevoked(r) => ServerEvent::PairingRevoked {
            peer: device_id(&r.peer_device_id)?,
            signed: r.signed?,
        },
        S::Envelope(e) => ServerEvent::Envelope {
            from: device_id(&e.from)?,
            sealed: e.sealed.to_vec(),
            seq: e.server_seq,
        },
        S::RendezvousPeer(p) => ServerEvent::RendezvousPeer { joined: p.joined },
        S::RendezvousData(d) => ServerEvent::RendezvousData(d.data.to_vec()),
        S::Superseded(_) => ServerEvent::Superseded,
        _ => return None,
    })
}
