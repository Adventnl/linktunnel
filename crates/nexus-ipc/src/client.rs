use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use nexus_proto::v1::{
    CloseSession, DeviceList, DirListing, FileEntry, FileList, FileMkdir, FileRemove,
    FileStatRequest, FileTransfer, GetStatus, IpcErrorCode, IpcEvent, IpcFrame, IpcHello,
    IpcRequest, IpcResponse, IpcWelcome, ListDevices, OpenSession, PairingCancel, PairingConfirm,
    PairingJoin, PairingOffer, PairingStarted, PingDevice, PingReport, SessionInput,
    SessionKeyframe, SessionOpened, SessionQuality, SetLockdown, Shutdown, Status, TransferStarted,
    Unpair, ipc_event::Body as Ev, ipc_frame::Body as F, ipc_request::Body as Req,
    ipc_response::Body as Resp,
};
use parking_lot::Mutex as SyncMutex;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::timeout;
use tracing::debug;

use crate::IpcError;
use crate::conn::{BoxedStream, IpcConnection, IpcWriter};
use crate::paths::{IpcEndpoint, IpcPaths};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Default per-request timeout; long operations pass their own.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

type Pending = Arc<SyncMutex<HashMap<u64, oneshot::Sender<IpcResponse>>>>;

/// Receiver of pushed [`IpcEvent`]s.
#[derive(Debug)]
pub struct EventReceiver(mpsc::Receiver<IpcEvent>);

impl EventReceiver {
    /// Next event; `None` once the agent connection is gone.
    pub async fn next(&mut self) -> Option<IpcEvent> {
        self.0.recv().await
    }
}

/// A connection to the local agent.
#[derive(Clone)]
pub struct IpcClient {
    writer: Arc<Mutex<IpcWriter>>,
    pending: Pending,
    next_id: Arc<AtomicU64>,
}

impl std::fmt::Debug for IpcClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("IpcClient")
    }
}

async fn open_stream(endpoint: &IpcEndpoint) -> Result<BoxedStream, IpcError> {
    match endpoint {
        #[cfg(unix)]
        IpcEndpoint::Unix(path) => {
            let stream = tokio::net::UnixStream::connect(path)
                .await
                .map_err(|e| IpcError::Unreachable(format!("{}: {e}", path.display())))?;
            Ok(Box::new(stream))
        }
        #[cfg(windows)]
        IpcEndpoint::Pipe(name) => {
            use tokio::net::windows::named_pipe::ClientOptions;
            // ERROR_PIPE_BUSY (231) means "all instances busy right now": retry briefly.
            for _ in 0..20 {
                match ClientOptions::new().open(name) {
                    Ok(c) => return Ok(Box::new(c)),
                    Err(e) if e.raw_os_error() == Some(231) => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    Err(e) => return Err(IpcError::Unreachable(format!("{name}: {e}"))),
                }
            }
            Err(IpcError::Unreachable(format!("{name}: busy")))
        }
        #[allow(unreachable_patterns)]
        _ => Err(IpcError::Protocol(
            "endpoint kind does not match this platform",
        )),
    }
}

impl IpcClient {
    /// Connects to the agent described by `paths` and authenticates as `client_name`.
    pub async fn connect(
        paths: &IpcPaths,
        client_name: &str,
    ) -> Result<(Self, EventReceiver, IpcWelcome), IpcError> {
        let token = std::fs::read_to_string(&paths.token_file)
            .map_err(|e| IpcError::Unreachable(format!("{}: {e}", paths.token_file.display())))?;
        let stream = timeout(CONNECT_TIMEOUT, open_stream(&paths.endpoint))
            .await
            .map_err(|_| IpcError::Timeout)??;
        let mut conn = IpcConnection::new(stream);
        conn.send(&IpcFrame {
            body: Some(F::Hello(IpcHello {
                token: token.trim().to_owned(),
                client: client_name.to_owned(),
            })),
        })
        .await?;
        let welcome = match timeout(CONNECT_TIMEOUT, conn.recv())
            .await
            .map_err(|_| IpcError::Timeout)??
        {
            Some(IpcFrame {
                body: Some(F::Welcome(w)),
            }) => w,
            None => return Err(IpcError::Unauthorized),
            _ => return Err(IpcError::Protocol("expected welcome")),
        };

        let (mut reader, writer) = conn.split();
        let pending: Pending = Arc::default();
        let (events_tx, events_rx) = mpsc::channel(256);
        let pending_for_reader = Arc::clone(&pending);
        tokio::spawn(async move {
            while let Ok(Some(frame)) = reader.recv().await {
                match frame.body {
                    Some(F::Response(r)) => {
                        if let Some(tx) = pending_for_reader.lock().remove(&r.in_reply_to) {
                            let _ = tx.send(r);
                        }
                    }
                    Some(F::Event(e)) => {
                        // Video and shapes must not be dropped silently (a lost frame breaks the
                        // decoder chain), so those wait for room; the reader then stops reading
                        // the socket, which backs pressure up to the agent. Everything else is
                        // droppable state that is resent on change.
                        if matches!(
                            e.body,
                            Some(
                                Ev::SessionVideo(_)
                                    | Ev::SessionCursorShape(_)
                                    | Ev::SessionEnded(_)
                            )
                        ) {
                            if events_tx.send(e).await.is_err() {
                                break;
                            }
                        } else {
                            let _ = events_tx.try_send(e);
                        }
                    }
                    _ => {}
                }
            }
            debug!("ipc client reader ended");
            // Dropping the pending senders wakes every waiter with `Closed`.
            pending_for_reader.lock().clear();
        });

        Ok((
            Self {
                writer: Arc::new(Mutex::new(writer)),
                pending,
                next_id: Arc::new(AtomicU64::new(1)),
            },
            EventReceiver(events_rx),
            welcome,
        ))
    }

    /// Sends a request and waits for its response.
    pub async fn request_with_timeout(&self, body: Req, limit: Duration) -> Result<Resp, IpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id, tx);
        let frame = IpcFrame {
            body: Some(F::Request(IpcRequest {
                id,
                body: Some(body),
            })),
        };
        if let Err(e) = self.writer.lock().await.send(&frame).await {
            self.pending.lock().remove(&id);
            return Err(e);
        }
        let response = match timeout(limit, rx).await {
            Err(_) => {
                self.pending.lock().remove(&id);
                return Err(IpcError::Timeout);
            }
            Ok(Err(_)) => return Err(IpcError::Closed),
            Ok(Ok(r)) => r,
        };
        match response.body {
            Some(Resp::Error(e)) => Err(IpcError::Agent {
                code: IpcErrorCode::try_from(e.code).unwrap_or(IpcErrorCode::Unspecified),
                message: e.message,
            }),
            Some(body) => Ok(body),
            None => Err(IpcError::Protocol("empty response")),
        }
    }

    /// [`request_with_timeout`](Self::request_with_timeout) with the default timeout.
    pub async fn request(&self, body: Req) -> Result<Resp, IpcError> {
        self.request_with_timeout(body, DEFAULT_TIMEOUT).await
    }

    async fn expect_ack(&self, body: Req) -> Result<(), IpcError> {
        match self.request(body).await? {
            Resp::Ack(_) => Ok(()),
            _ => Err(IpcError::Protocol("unexpected response")),
        }
    }

    /// Agent status.
    pub async fn status(&self) -> Result<Status, IpcError> {
        match self.request(Req::Status(GetStatus {})).await? {
            Resp::Status(s) => Ok(s),
            _ => Err(IpcError::Protocol("unexpected response")),
        }
    }

    /// Paired devices with their presence.
    pub async fn list_devices(&self) -> Result<DeviceList, IpcError> {
        match self.request(Req::ListDevices(ListDevices {})).await? {
            Resp::Devices(d) => Ok(d),
            _ => Err(IpcError::Protocol("unexpected response")),
        }
    }

    /// Starts showing a pairing code.
    pub async fn pairing_offer(
        &self,
        allow_peer_to: Vec<String>,
    ) -> Result<PairingStarted, IpcError> {
        match self
            .request(Req::PairingOffer(PairingOffer { allow_peer_to }))
            .await?
        {
            Resp::PairingStarted(p) => Ok(p),
            _ => Err(IpcError::Protocol("unexpected response")),
        }
    }

    /// Enters a pairing code shown by another device.
    pub async fn pairing_join(
        &self,
        code: &str,
        request_on_peer: Vec<String>,
        grant_to_peer: Vec<String>,
    ) -> Result<PairingStarted, IpcError> {
        let body = Req::PairingJoin(PairingJoin {
            code: code.to_owned(),
            request_on_peer,
            grant_to_peer,
        });
        match self.request(body).await? {
            Resp::PairingStarted(p) => Ok(p),
            _ => Err(IpcError::Protocol("unexpected response")),
        }
    }

    /// Reports whether the words on both screens matched.
    pub async fn pairing_confirm(
        &self,
        pairing_id: u64,
        words_match: bool,
    ) -> Result<(), IpcError> {
        self.expect_ack(Req::PairingConfirm(PairingConfirm {
            pairing_id,
            words_match,
        }))
        .await
    }

    /// Abandons a pairing in progress.
    pub async fn pairing_cancel(&self, pairing_id: u64) -> Result<(), IpcError> {
        self.expect_ack(Req::PairingCancel(PairingCancel { pairing_id }))
            .await
    }

    /// Removes a pairing (revokes it on the server and closes live sessions).
    pub async fn unpair(&self, device_id: Vec<u8>) -> Result<(), IpcError> {
        self.expect_ack(Req::Unpair(Unpair {
            device_id: device_id.into(),
        }))
        .await
    }

    /// Runs the diagnostics suite against a paired device.
    pub async fn ping(&self, request: PingDevice) -> Result<PingReport, IpcError> {
        match self
            .request_with_timeout(Req::Ping(request), Duration::from_secs(300))
            .await?
        {
            Resp::PingReport(r) => Ok(r),
            _ => Err(IpcError::Protocol("unexpected response")),
        }
    }

    /// Turns the lockdown switch on or off.
    pub async fn set_lockdown(&self, enabled: bool) -> Result<(), IpcError> {
        self.expect_ack(Req::SetLockdown(SetLockdown { enabled }))
            .await
    }

    /// Asks the agent to exit.
    pub async fn shutdown(&self) -> Result<(), IpcError> {
        self.expect_ack(Req::Shutdown(Shutdown {})).await
    }

    /// Opens a remote-desktop session to a paired device. Video and cursor arrive as events.
    pub async fn open_session(&self, request: OpenSession) -> Result<SessionOpened, IpcError> {
        match self
            .request_with_timeout(Req::OpenSession(request), Duration::from_secs(30))
            .await?
        {
            Resp::SessionOpened(s) => Ok(s),
            _ => Err(IpcError::Protocol("unexpected response")),
        }
    }

    /// Ends a session.
    pub async fn close_session(&self, session_id: u64) -> Result<(), IpcError> {
        self.expect_ack(Req::CloseSession(CloseSession { session_id }))
            .await
    }

    /// Sends input events (an `InputPacket` in wire format) into a session.
    pub async fn session_input(&self, session_id: u64, packet: Vec<u8>) -> Result<(), IpcError> {
        self.expect_ack(Req::SessionInput(SessionInput {
            session_id,
            packet: packet.into(),
        }))
        .await
    }

    /// Asks for a recovery frame (the decoder choked).
    pub async fn session_keyframe(&self, session_id: u64) -> Result<(), IpcError> {
        self.expect_ack(Req::SessionKeyframe(SessionKeyframe { session_id }))
            .await
    }

    /// Changes quality settings (0 = unchanged).
    pub async fn session_quality(&self, request: SessionQuality) -> Result<(), IpcError> {
        self.expect_ack(Req::SessionQuality(request)).await
    }

    /// Lists a directory on the remote device (empty path: its shared folders).
    pub async fn file_list(&self, session_id: u64, path: &str) -> Result<DirListing, IpcError> {
        match self
            .request(Req::FileList(FileList {
                session_id,
                path: path.to_owned(),
            }))
            .await?
        {
            Resp::FileListing(l) => Ok(l),
            _ => Err(IpcError::Protocol("unexpected response")),
        }
    }

    /// Metadata of one remote path.
    pub async fn file_stat(&self, session_id: u64, path: &str) -> Result<FileEntry, IpcError> {
        match self
            .request(Req::FileStat(FileStatRequest {
                session_id,
                path: path.to_owned(),
            }))
            .await?
        {
            Resp::FileEntry(e) => Ok(e),
            _ => Err(IpcError::Protocol("unexpected response")),
        }
    }

    /// Creates a remote directory.
    pub async fn file_mkdir(&self, session_id: u64, path: &str) -> Result<(), IpcError> {
        self.expect_ack(Req::FileMkdir(FileMkdir {
            session_id,
            path: path.to_owned(),
        }))
        .await
    }

    /// Deletes a remote file or directory.
    pub async fn file_remove(
        &self,
        session_id: u64,
        path: &str,
        recursive: bool,
    ) -> Result<(), IpcError> {
        self.expect_ack(Req::FileRemove(FileRemove {
            session_id,
            path: path.to_owned(),
            recursive,
        }))
        .await
    }

    /// Starts copying one file; progress and completion arrive as events.
    pub async fn file_transfer(&self, request: FileTransfer) -> Result<TransferStarted, IpcError> {
        match self.request(Req::FileTransfer(request)).await? {
            Resp::TransferStarted(t) => Ok(t),
            _ => Err(IpcError::Protocol("unexpected response")),
        }
    }
}
