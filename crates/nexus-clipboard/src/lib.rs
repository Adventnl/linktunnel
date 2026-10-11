//! Clipboard synchronisation.
//!
//! One long-lived stream per session carries [`ClipboardUpdate`](nexus_proto::v1::ClipboardUpdate)s
//! in both directions. Each side watches its own clipboard (polling: portable, and cheap because
//! only a hash is compared), pushes a new value when the local user copies something, and applies
//! the other side's pushes. A hash of the last value seen or applied stops echoes, so the two
//! clipboards settle instead of ping-ponging.
//!
//! Each direction is an independent opt-in ([`ClipboardPolicy`]), and the size of one value is
//! capped. Content is never interpreted: text is UTF-8 and images are PNG, nothing else is
//! accepted.

#![forbid(unsafe_code)]

mod backend;
mod content;
mod error;
mod io;
mod sync;

pub use backend::{
    ArboardBackend, ClipboardBackend, ClipboardFactory, ClipboardSetup, MemoryClipboard,
    os_clipboard,
};
pub use content::{ClipContent, MAX_CLIPBOARD_BYTES, decode_png, encode_png};
pub use error::ClipError;
pub use io::{ClipIo, MemoryClipIo, memory_pair};
pub use sync::{ClipboardPolicy, run};
