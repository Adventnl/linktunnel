//! Local IPC between the agent and its clients.
//!
//! **Why not just run everything in one process?** The agent owns the device identity, the
//! signaling connection and every live session, and must keep running when no UI is open (it is a
//! boot-time service). The CLI, tray and app are short-lived and unprivileged, so they talk to
//! the agent over this channel instead of holding keys.
//!
//! **Authentication.** Two independent checks, both required:
//!
//! 1. *Filesystem access.* The socket and the token file live in the user's private runtime
//!    directory (mode `0700`/`0600`); only that user (and root) can open them.
//! 2. *Possession of the token.* The client sends the random token (read from that private file)
//!    as its first frame; on Unix the server also verifies the peer's UID.
//!
//! On Windows the named pipe has the default ACL, so the token check is what keeps other local
//! users out (the token file sits in the user's profile, which is ACL-protected).

#![deny(unsafe_code)]

mod client;
mod conn;
mod paths;
mod server;

pub use client::{EventReceiver, IpcClient};
pub use conn::{IpcConnection, MAX_IPC_FRAME};
pub use paths::{IpcEndpoint, IpcPaths};
pub use server::{IpcServer, PendingConnection};

use nexus_proto::framing::FrameError;

/// IPC failures.
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    /// Socket / file I/O.
    #[error("ipc I/O: {0}")]
    Io(#[from] std::io::Error),
    /// Another agent already owns the endpoint.
    #[error("an agent is already running (socket {0} is live)")]
    AlreadyRunning(String),
    /// The agent is not running or not reachable.
    #[error("cannot reach the agent: {0}")]
    Unreachable(String),
    /// The peer failed authentication.
    #[error("ipc authentication failed")]
    Unauthorized,
    /// The peer violated the protocol.
    #[error("ipc protocol error: {0}")]
    Protocol(&'static str),
    /// A frame was malformed or oversized.
    #[error(transparent)]
    Frame(#[from] FrameError),
    /// The connection is closed.
    #[error("ipc connection closed")]
    Closed,
    /// The agent answered with an error.
    #[error("agent error {code:?}: {message}")]
    Agent {
        /// Machine-readable reason.
        code: nexus_proto::v1::IpcErrorCode,
        /// Human-readable text.
        message: String,
    },
    /// No reply in time.
    #[error("timed out")]
    Timeout,
}

#[cfg(test)]
mod tests;
