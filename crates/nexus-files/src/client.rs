//! The controller side: browse, download and upload with resume.

use std::path::{Path, PathBuf};

use nexus_proto::v1::{
    DirListing, FileEntry, ListDir, MakeDir, ReadFile, RemovePath, StatPath, UploadEnd,
    UploadStart, WriteFile, file_message::Body,
};
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::common::{CHUNK, chunk, hash_matches, hash_prefix, msg, part_path, unix_ms};
use crate::error::FileError;
use crate::io::{MsgIo, Opener};

/// How far a transfer has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Bytes of the file now in place (including a resumed prefix).
    pub done: u64,
    /// File size.
    pub total: u64,
}

/// What a finished transfer produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transferred {
    /// File size.
    pub size: u64,
    /// BLAKE3 of the whole file, verified on both sides.
    pub blake3: [u8; 32],
    /// Where transfer started: non-zero when a partial copy was continued.
    pub resumed_from: u64,
}

/// A client of one remote device's file service.
#[derive(Debug, Clone)]
pub struct FilesClient<O> {
    opener: O,
}

impl<O: Opener> FilesClient<O> {
    /// A client opening its operation channels with `opener`.
    #[must_use]
    pub fn new(opener: O) -> Self {
        Self { opener }
    }

    /// The shared folders (`path` empty) or the contents of a directory.
    pub async fn list(&self, path: &str) -> Result<DirListing, FileError> {
        let mut io = self.opener.open().await?;
        io.send(msg(Body::List(ListDir { path: path.into() })))
            .await?;
        match reply(&mut io).await? {
            Body::Listing(l) => Ok(l),
            _ => Err(unexpected()),
        }
    }

    /// Metadata of one path.
    pub async fn stat(&self, path: &str) -> Result<FileEntry, FileError> {
        let mut io = self.opener.open().await?;
        io.send(msg(Body::Stat(StatPath { path: path.into() })))
            .await?;
        match reply(&mut io).await? {
            Body::FileStat(s) => s.entry.ok_or_else(unexpected),
            _ => Err(unexpected()),
        }
    }

    /// Creates a directory (the parent must exist).
    pub async fn mkdir(&self, path: &str) -> Result<(), FileError> {
        let mut io = self.opener.open().await?;
        io.send(msg(Body::Mkdir(MakeDir { path: path.into() })))
            .await?;
        expect_done(&mut io).await
    }

    /// Deletes a file, a symlink or (with `recursive`) a directory tree.
    pub async fn remove(&self, path: &str, recursive: bool) -> Result<(), FileError> {
        let mut io = self.opener.open().await?;
        io.send(msg(Body::Remove(RemovePath {
            path: path.into(),
            recursive,
        })))
        .await?;
        expect_done(&mut io).await
    }

    /// Downloads `remote` to `dest`. Data lands in `dest` + `.nexus-part` and is renamed into
    /// place after the BLAKE3 of the whole file has been verified; a leftover partial file is
    /// continued if (and only if) it is a true prefix of the remote file.
    pub async fn download(
        &self,
        remote: &str,
        dest: &Path,
        overwrite: bool,
        mut progress: impl FnMut(Progress) + Send,
    ) -> Result<Transferred, FileError> {
        if !overwrite && fs::symlink_metadata(dest).await.is_ok() {
            return Err(FileError::AlreadyExists(dest.display().to_string()));
        }
        let part = part_path(dest);
        let mut have = 0u64;
        let mut have_hash = blake3::Hasher::new();
        if let Ok(mut f) = File::open(&part).await {
            let len = f.metadata().await.map(|m| m.len()).unwrap_or(0);
            if len > 0 {
                have_hash = hash_prefix(&mut f, len).await?;
                have = len;
            }
        }
        let mut io = self.opener.open().await?;
        io.send(msg(Body::Read(ReadFile {
            path: remote.into(),
            resume_from: have,
            prefix_blake3: if have > 0 {
                have_hash.clone().finalize().as_bytes().to_vec().into()
            } else {
                Vec::new().into()
            },
        })))
        .await?;
        let Body::ReadStart(start) = reply(&mut io).await? else {
            return Err(unexpected());
        };
        if start.start != 0 && start.start != have {
            return Err(FileError::Protocol(
                "server resumed from a different offset".into(),
            ));
        }
        let hasher_start = if start.start == 0 {
            blake3::Hasher::new()
        } else {
            have_hash
        };
        let mut hasher = hasher_start;
        let mut out = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&part)
            .await
            .map_err(|e| FileError::from_io(&e, &part.display().to_string()))?;
        out.set_len(start.start)
            .await
            .map_err(|e| FileError::from_io(&e, "truncate"))?;
        out.seek(std::io::SeekFrom::Start(start.start))
            .await
            .map_err(|e| FileError::from_io(&e, "seek"))?;
        let mut got = start.start;
        progress(Progress {
            done: got,
            total: start.size,
        });
        let done = loop {
            let Some(m) = io.recv().await? else {
                out.flush().await.ok();
                return Err(FileError::Protocol("download interrupted".into()));
            };
            match m.body {
                Some(Body::Chunk(c)) => {
                    got += c.data.len() as u64;
                    if got > start.size {
                        drop(out);
                        let _ = fs::remove_file(&part).await;
                        return Err(FileError::Protocol("more data than announced".into()));
                    }
                    hasher.update(&c.data);
                    out.write_all(&c.data)
                        .await
                        .map_err(|e| FileError::from_io(&e, "write"))?;
                    progress(Progress {
                        done: got,
                        total: start.size,
                    });
                }
                Some(Body::Done(d)) => break d,
                Some(Body::Error(e)) => {
                    out.flush().await.ok();
                    return Err(FileError::from_wire(&e));
                }
                _ => return Err(unexpected()),
            }
        };
        let digest = hasher.finalize();
        if got != start.size || got != done.size || !hash_matches(&digest, &done.blake3) {
            drop(out);
            let _ = fs::remove_file(&part).await;
            return Err(FileError::HashMismatch(remote.to_owned()));
        }
        out.flush()
            .await
            .map_err(|e| FileError::from_io(&e, "flush"))?;
        out.sync_all()
            .await
            .map_err(|e| FileError::from_io(&e, "sync"))?;
        if start.mtime_unix_ms > 0 {
            let when =
                std::time::UNIX_EPOCH + std::time::Duration::from_millis(start.mtime_unix_ms);
            let std_file = out.into_std().await;
            let _ = std_file.set_modified(when);
        } else {
            drop(out);
        }
        fs::rename(&part, dest)
            .await
            .map_err(|e| FileError::from_io(&e, &dest.display().to_string()))?;
        Ok(Transferred {
            size: got,
            blake3: *digest.as_bytes(),
            resumed_from: start.start,
        })
    }

    /// Uploads `local` to `remote`, continuing a partial copy the target still holds if it is a
    /// true prefix of this file.
    pub async fn upload(
        &self,
        local: &Path,
        remote: &str,
        overwrite: bool,
        mut progress: impl FnMut(Progress) + Send,
    ) -> Result<Transferred, FileError> {
        let mut file = File::open(local)
            .await
            .map_err(|e| FileError::from_io(&e, &local.display().to_string()))?;
        let meta = file
            .metadata()
            .await
            .map_err(|e| FileError::from_io(&e, &local.display().to_string()))?;
        if meta.is_dir() {
            return Err(FileError::IsADirectory(local.display().to_string()));
        }
        let size = meta.len();
        let mut io = self.opener.open().await?;
        io.send(msg(Body::Write(WriteFile {
            path: remote.into(),
            size,
            overwrite,
            mtime_unix_ms: unix_ms(meta.modified()),
        })))
        .await?;
        let Body::WriteStart(ws) = reply(&mut io).await? else {
            return Err(unexpected());
        };
        let mut start = 0;
        let mut hasher = blake3::Hasher::new();
        if ws.have > 0 && ws.have <= size {
            let h = hash_prefix(&mut file, ws.have).await?;
            if hash_matches(&h.clone().finalize(), &ws.have_blake3) {
                hasher = h;
                start = ws.have;
            }
        }
        if start == 0 {
            file.seek(std::io::SeekFrom::Start(0))
                .await
                .map_err(|e| FileError::from_io(&e, "seek"))?;
        }
        io.send(msg(Body::UploadStart(UploadStart { start })))
            .await?;
        let mut sent = start;
        progress(Progress {
            done: sent,
            total: size,
        });
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = file
                .read(&mut buf)
                .await
                .map_err(|e| FileError::from_io(&e, "read"))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            sent += n as u64;
            io.send(chunk(&buf[..n])).await?;
            progress(Progress {
                done: sent,
                total: size,
            });
        }
        if sent != size {
            return Err(FileError::Io(format!(
                "{} changed while it was uploaded",
                local.display()
            )));
        }
        let digest = hasher.finalize();
        io.send(msg(Body::UploadEnd(UploadEnd {
            blake3: digest.as_bytes().to_vec().into(),
        })))
        .await?;
        let Body::Done(done) = reply(&mut io).await? else {
            return Err(unexpected());
        };
        if done.size != size || !hash_matches(&digest, &done.blake3) {
            return Err(FileError::HashMismatch(remote.to_owned()));
        }
        io.finish()?;
        Ok(Transferred {
            size,
            blake3: *digest.as_bytes(),
            resumed_from: start,
        })
    }
}

/// Convenience: a local destination path for `remote_name` inside `dir`.
#[must_use]
pub fn local_destination(dir: &Path, remote_name: &str) -> PathBuf {
    let name = Path::new(remote_name)
        .file_name()
        .map_or_else(|| "download".into(), std::ffi::OsStr::to_os_string);
    dir.join(name)
}

fn unexpected() -> FileError {
    FileError::Protocol("unexpected reply".into())
}

/// Next message, turning a wire error into `Err`.
async fn reply<I: MsgIo>(io: &mut I) -> Result<Body, FileError> {
    match io.recv().await? {
        Some(m) => match m.body {
            Some(Body::Error(e)) => Err(FileError::from_wire(&e)),
            Some(b) => Ok(b),
            None => Err(unexpected()),
        },
        None => Err(FileError::Protocol("the device closed the stream".into())),
    }
}

async fn expect_done<I: MsgIo>(io: &mut I) -> Result<(), FileError> {
    match reply(io).await? {
        Body::Done(_) => Ok(()),
        _ => Err(unexpected()),
    }
}
