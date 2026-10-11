//! Small helpers shared by the server and the client.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use nexus_proto::v1::{FileChunk, FileMessage, file_message::Body};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::error::FileError;

/// Payload bytes per data message.
pub const CHUNK: usize = 256 * 1024;
/// Suffix of a file being received.
pub const PART_SUFFIX: &str = ".nexus-part";

/// Wraps a body into a message.
#[must_use]
pub fn msg(body: Body) -> FileMessage {
    FileMessage { body: Some(body) }
}

/// A data message.
#[must_use]
pub fn chunk(data: &[u8]) -> FileMessage {
    msg(Body::Chunk(FileChunk {
        data: data.to_vec().into(),
    }))
}

/// Where the in-progress copy of `final_path` lives.
#[must_use]
pub fn part_path(final_path: &Path) -> PathBuf {
    let mut s = final_path.as_os_str().to_owned();
    s.push(PART_SUFFIX);
    PathBuf::from(s)
}

/// Milliseconds since the Unix epoch (0 if unknown).
#[must_use]
pub fn unix_ms(t: std::io::Result<SystemTime>) -> u64 {
    t.ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0))
}

/// Hashes the first `len` bytes of `file` (from its start) and returns the hasher, positioned so
/// that more bytes can be fed in; the file cursor ends at `len`.
pub async fn hash_prefix(file: &mut File, len: u64) -> Result<blake3::Hasher, FileError> {
    file.seek(std::io::SeekFrom::Start(0))
        .await
        .map_err(|e| FileError::from_io(&e, "seek"))?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; CHUNK];
    let mut left = len;
    while left > 0 {
        let want = usize::try_from(left).unwrap_or(usize::MAX).min(CHUNK);
        let n = file
            .read(&mut buf[..want])
            .await
            .map_err(|e| FileError::from_io(&e, "read"))?;
        if n == 0 {
            return Err(FileError::Io("file is shorter than expected".into()));
        }
        hasher.update(&buf[..n]);
        left -= n as u64;
    }
    Ok(hasher)
}

/// Constant-length comparison of a hash with wire bytes.
#[must_use]
pub fn hash_matches(h: &blake3::Hash, wire: &[u8]) -> bool {
    wire.len() == 32 && h.as_bytes().as_slice() == wire
}
