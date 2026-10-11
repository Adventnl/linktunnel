//! File browsing and transfer between paired devices.
//!
//! One QUIC stream per operation (`StreamKind::Files`), so a large copy never blocks the video or
//! input streams and several can run at once. The target only serves the folders it has chosen to
//! share ([`FileRoots`]); transfers are resumable and verified end to end with BLAKE3.
//!
//! * [`FileServer`]: answers operations on the target.
//! * [`FilesClient`]: the controller's API (`list`, `stat`, `mkdir`, `remove`, `download`,
//!   `upload`).
//! * [`MsgIo`] / [`Opener`]: the transport seam; QUIC in production, in-memory in tests.

#![forbid(unsafe_code)]

mod client;
mod common;
mod error;
mod io;
mod policy;
mod server;

pub use client::{FilesClient, Progress, Transferred, local_destination};
pub use common::{CHUNK, PART_SUFFIX, part_path};
pub use error::FileError;
pub use io::{MemoryIo, MsgIo, Opener, framed, memory_pair};
pub use policy::{Access, FileRoots, SharedRoot};
pub use server::{FileServer, MAX_ENTRIES};
