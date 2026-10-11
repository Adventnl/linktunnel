//! Errors, and their mapping to the wire's [`FileErrorCode`].

use std::io;

use nexus_proto::v1::{FileError as WireError, FileErrorCode};

/// Why a file operation failed.
#[derive(Debug, thiserror::Error)]
pub enum FileError {
    /// The path does not exist.
    #[error("not found: {0}")]
    NotFound(String),
    /// The path is outside the shared folders, or the folder is read-only.
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    /// The destination exists and overwriting was not requested.
    #[error("already exists: {0}")]
    AlreadyExists(String),
    /// The path is malformed (relative, contains `..`, empty name, ...).
    #[error("invalid path: {0}")]
    InvalidPath(String),
    /// A local I/O error.
    #[error("i/o error: {0}")]
    Io(String),
    /// The file is larger than this device accepts.
    #[error("file too large: {0}")]
    TooLarge(String),
    /// The received bytes do not hash to what the sender promised.
    #[error("hash mismatch: {0}")]
    HashMismatch(String),
    /// Another transfer is already writing this file.
    #[error("busy: {0}")]
    Busy(String),
    /// A directory was expected.
    #[error("not a directory: {0}")]
    NotADirectory(String),
    /// A file was expected.
    #[error("is a directory: {0}")]
    IsADirectory(String),
    /// The peer broke the protocol or the stream died.
    #[error("protocol error: {0}")]
    Protocol(String),
}

impl FileError {
    /// The wire code.
    #[must_use]
    pub fn code(&self) -> FileErrorCode {
        match self {
            Self::NotFound(_) => FileErrorCode::NotFound,
            Self::PermissionDenied(_) => FileErrorCode::PermissionDenied,
            Self::AlreadyExists(_) => FileErrorCode::AlreadyExists,
            Self::InvalidPath(_) => FileErrorCode::InvalidPath,
            Self::Io(_) | Self::Protocol(_) => FileErrorCode::Io,
            Self::TooLarge(_) => FileErrorCode::TooLarge,
            Self::HashMismatch(_) => FileErrorCode::HashMismatch,
            Self::Busy(_) => FileErrorCode::Busy,
            Self::NotADirectory(_) => FileErrorCode::NotADirectory,
            Self::IsADirectory(_) => FileErrorCode::IsADirectory,
        }
    }

    /// The wire form. Local detail beyond the message is never sent.
    #[must_use]
    pub fn to_wire(&self) -> WireError {
        WireError {
            code: self.code() as i32,
            message: self.to_string(),
        }
    }

    /// Rebuilds an error received from the peer.
    #[must_use]
    pub fn from_wire(w: &WireError) -> Self {
        let m = w.message.clone();
        match FileErrorCode::try_from(w.code).unwrap_or(FileErrorCode::Unspecified) {
            FileErrorCode::NotFound => Self::NotFound(m),
            FileErrorCode::PermissionDenied => Self::PermissionDenied(m),
            FileErrorCode::AlreadyExists => Self::AlreadyExists(m),
            FileErrorCode::InvalidPath => Self::InvalidPath(m),
            FileErrorCode::TooLarge => Self::TooLarge(m),
            FileErrorCode::HashMismatch => Self::HashMismatch(m),
            FileErrorCode::Busy => Self::Busy(m),
            FileErrorCode::NotADirectory => Self::NotADirectory(m),
            FileErrorCode::IsADirectory => Self::IsADirectory(m),
            FileErrorCode::Io | FileErrorCode::Unspecified => Self::Io(m),
        }
    }

    /// Maps an OS error for `what`.
    #[must_use]
    pub fn from_io(e: &io::Error, what: &str) -> Self {
        match e.kind() {
            io::ErrorKind::NotFound => Self::NotFound(what.to_owned()),
            io::ErrorKind::PermissionDenied => Self::PermissionDenied(what.to_owned()),
            io::ErrorKind::AlreadyExists => Self::AlreadyExists(what.to_owned()),
            _ => Self::Io(format!("{what}: {e}")),
        }
    }
}

impl From<nexus_net::NetError> for FileError {
    fn from(e: nexus_net::NetError) -> Self {
        Self::Protocol(e.to_string())
    }
}
