//! Terminal sessions: a PTY-hosted shell on the target, driven from a controller.
//!
//! * [`TerminalHost`] (target side) starts shells, keeps named ones alive when the controller
//!   disconnects (so a flaky connection does not kill a long build) and replays recent output on
//!   re-attach, much like `tmux attach` without tmux.
//! * [`serve`] runs one attached terminal over a message channel ([`TermIo`]).
//! * [`run_client`] is the controller end of that channel.
//!
//! The channel is abstract so this crate has no network dependency and can be tested (and
//! type-checked for Windows, where the PTY is ConPTY) on its own.

#![forbid(unsafe_code)]

mod client;
mod host;
mod io;
mod serve;

pub use client::{TermCmd, TermEvent, run_client};
pub use host::{HostConfig, TerminalHost, default_shell};
pub use io::{MemoryTermIo, TermIo, memory_pair};
pub use serve::serve;

/// Why a terminal operation failed.
#[derive(Debug, thiserror::Error)]
pub enum TermError {
    /// The shell could not be started.
    #[error("could not start the shell: {0}")]
    Spawn(String),
    /// The requested program is not one this device allows.
    #[error("{0} is not an allowed shell on this device")]
    ShellNotAllowed(String),
    /// Too many terminals are open.
    #[error("too many terminals are open on this device")]
    TooMany,
    /// No such persistent terminal (and `reattach_only` was set).
    #[error("there is no terminal named {0}")]
    NoSuchSession(String),
    /// The peer broke the protocol or the stream died.
    #[error("terminal stream: {0}")]
    Stream(String),
}
