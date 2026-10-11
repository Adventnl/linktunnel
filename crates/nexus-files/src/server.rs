//! The target side: answers one file operation per stream.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use nexus_proto::v1::{
    DirListing, FileDone, FileEntry, FileMessage, FileStat, MakeDir, ReadFile, ReadStart,
    RemovePath, StatPath, WriteFile, WriteStart, file_message::Body,
};
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tracing::{debug, warn};

use crate::common::{CHUNK, chunk, hash_matches, hash_prefix, msg, part_path, unix_ms};
use crate::error::FileError;
use crate::io::MsgIo;
use crate::policy::{Access, FileRoots};

/// Directory entries returned per listing.
pub const MAX_ENTRIES: usize = 20_000;

/// Serves file operations.
#[derive(Debug, Clone)]
pub struct FileServer {
    roots: FileRoots,
    max_upload: u64,
    writing: Arc<busy::Busy>,
}

/// A tiny mutex-protected set, kept private so the crate needs no extra dependency.
mod busy {
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    pub struct Busy(Mutex<HashSet<PathBuf>>);

    impl Busy {
        pub fn try_lock(self: &std::sync::Arc<Self>, p: &Path) -> Option<Guard> {
            let mut set = self.0.lock().ok()?;
            set.insert(p.to_path_buf()).then(|| Guard {
                owner: std::sync::Arc::clone(self),
                path: p.to_path_buf(),
            })
        }
    }

    #[derive(Debug)]
    pub struct Guard {
        owner: std::sync::Arc<Busy>,
        path: PathBuf,
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            if let Ok(mut set) = self.owner.0.lock() {
                set.remove(&self.path);
            }
        }
    }
}

impl FileServer {
    /// A server for `roots`; uploads larger than `max_upload` bytes are refused.
    #[must_use]
    pub fn new(roots: FileRoots, max_upload: u64) -> Self {
        Self {
            roots,
            max_upload,
            writing: Arc::default(),
        }
    }

    /// Handles the operation on `io`, replying with an error message if it fails.
    pub async fn serve<I: MsgIo>(&self, io: &mut I) -> Result<(), FileError> {
        let Some(first) = io.recv().await? else {
            return Ok(());
        };
        let result = match first.body {
            Some(Body::List(r)) => self.list(io, &r.path).await,
            Some(Body::Stat(r)) => self.stat(io, r).await,
            Some(Body::Read(r)) => self.read(io, r).await,
            Some(Body::Write(r)) => self.write(io, r).await,
            Some(Body::Mkdir(r)) => self.mkdir(io, r).await,
            Some(Body::Remove(r)) => self.remove(io, r).await,
            _ => Err(FileError::Protocol("unexpected first message".into())),
        };
        match result {
            Ok(()) => io.finish(),
            Err(e) => {
                debug!(error = %e, "file operation failed");
                // The peer may already be gone; the error is only informational then.
                let _ = io.send(msg(Body::Error(e.to_wire()))).await;
                let _ = io.finish();
                Ok(())
            }
        }
    }

    async fn list<I: MsgIo>(&self, io: &mut I, wire: &str) -> Result<(), FileError> {
        if wire.is_empty() {
            let entries = self
                .roots
                .roots()
                .into_iter()
                .map(|(p, writable)| FileEntry {
                    name: p.display().to_string(),
                    is_dir: true,
                    readonly: !writable,
                    ..FileEntry::default()
                })
                .collect();
            return io
                .send(msg(Body::Listing(DirListing {
                    path: String::new(),
                    entries,
                    truncated: false,
                })))
                .await;
        }
        let dir = self.roots.resolve(wire, Access::Read)?;
        let mut rd = fs::read_dir(&dir)
            .await
            .map_err(|e| match e.raw_os_error() {
                // ENOTDIR / ERROR_DIRECTORY
                Some(20 | 267) => FileError::NotADirectory(wire.to_owned()),
                _ => FileError::from_io(&e, wire),
            })?;
        let mut entries = Vec::new();
        let mut truncated = false;
        while let Some(de) = rd
            .next_entry()
            .await
            .map_err(|e| FileError::from_io(&e, wire))?
        {
            if entries.len() >= MAX_ENTRIES {
                truncated = true;
                break;
            }
            if let Some(entry) =
                entry_of(&de.path(), de.file_name().to_string_lossy().into_owned()).await
            {
                entries.push(entry);
            }
        }
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        io.send(msg(Body::Listing(DirListing {
            path: dir.display().to_string(),
            entries,
            truncated,
        })))
        .await
    }

    async fn stat<I: MsgIo>(&self, io: &mut I, r: StatPath) -> Result<(), FileError> {
        let p = self.roots.resolve(&r.path, Access::Read)?;
        let name = p.file_name().map_or_else(
            || p.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let entry = entry_of(&p, name)
            .await
            .ok_or_else(|| FileError::NotFound(r.path.clone()))?;
        io.send(msg(Body::FileStat(FileStat {
            path: p.display().to_string(),
            entry: Some(entry),
        })))
        .await
    }

    async fn mkdir<I: MsgIo>(&self, io: &mut I, r: MakeDir) -> Result<(), FileError> {
        let p = self.roots.resolve_new(&r.path, Access::Write)?;
        fs::create_dir(&p)
            .await
            .map_err(|e| FileError::from_io(&e, &r.path))?;
        io.send(msg(Body::Done(FileDone::default()))).await
    }

    async fn remove<I: MsgIo>(&self, io: &mut I, r: RemovePath) -> Result<(), FileError> {
        let p = self.roots.resolve_entry(&r.path, Access::Write)?;
        if self.roots.is_root(&p) {
            return Err(FileError::PermissionDenied(
                "a shared folder itself cannot be removed".into(),
            ));
        }
        let meta = fs::symlink_metadata(&p)
            .await
            .map_err(|e| FileError::from_io(&e, &r.path))?;
        let res = if meta.is_dir() {
            if r.recursive {
                fs::remove_dir_all(&p).await
            } else {
                fs::remove_dir(&p).await
            }
        } else {
            // Files and symlinks (the link, never its target).
            fs::remove_file(&p).await
        };
        res.map_err(|e| FileError::from_io(&e, &r.path))?;
        io.send(msg(Body::Done(FileDone::default()))).await
    }

    async fn read<I: MsgIo>(&self, io: &mut I, r: ReadFile) -> Result<(), FileError> {
        let p = self.roots.resolve(&r.path, Access::Read)?;
        let mut file = File::open(&p)
            .await
            .map_err(|e| FileError::from_io(&e, &r.path))?;
        let meta = file
            .metadata()
            .await
            .map_err(|e| FileError::from_io(&e, &r.path))?;
        if meta.is_dir() {
            return Err(FileError::IsADirectory(r.path));
        }
        let size = meta.len();
        // Continue only if what the controller holds is a true prefix of this file.
        let mut start = 0;
        let mut hasher = blake3::Hasher::new();
        if r.resume_from > 0 && r.resume_from <= size {
            let h = hash_prefix(&mut file, r.resume_from).await?;
            if hash_matches(&h.finalize(), &r.prefix_blake3) {
                hasher = h;
                start = r.resume_from;
            }
        }
        if start == 0 {
            file.seek(std::io::SeekFrom::Start(0))
                .await
                .map_err(|e| FileError::from_io(&e, &r.path))?;
        }
        io.send(msg(Body::ReadStart(ReadStart {
            size,
            mtime_unix_ms: unix_ms(meta.modified()),
            start,
        })))
        .await?;
        let mut sent = start;
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = file
                .read(&mut buf)
                .await
                .map_err(|e| FileError::from_io(&e, &r.path))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            sent += n as u64;
            io.send(chunk(&buf[..n])).await?;
        }
        if sent != size {
            return Err(FileError::Io(format!(
                "{} changed while it was read",
                r.path
            )));
        }
        io.send(msg(Body::Done(FileDone {
            size,
            blake3: hasher.finalize().as_bytes().to_vec().into(),
        })))
        .await
    }

    async fn write<I: MsgIo>(&self, io: &mut I, r: WriteFile) -> Result<(), FileError> {
        if r.size > self.max_upload {
            return Err(FileError::TooLarge(format!(
                "{} bytes exceeds the {} byte limit",
                r.size, self.max_upload
            )));
        }
        let final_path = self.roots.resolve_new(&r.path, Access::Write)?;
        match fs::symlink_metadata(&final_path).await {
            Ok(m) if m.is_dir() => return Err(FileError::IsADirectory(r.path)),
            Ok(_) if !r.overwrite => return Err(FileError::AlreadyExists(r.path)),
            _ => {}
        }
        let _guard = self
            .writing
            .try_lock(&final_path)
            .ok_or_else(|| FileError::Busy(r.path.clone()))?;
        let part = part_path(&final_path);
        // A partial copy that is longer than the promised size cannot be a prefix.
        let (mut have, mut hasher) = (0u64, blake3::Hasher::new());
        if let Ok(mut f) = File::open(&part).await {
            let len = f.metadata().await.map(|m| m.len()).unwrap_or(0);
            if len > 0 && len <= r.size {
                hasher = hash_prefix(&mut f, len).await?;
                have = len;
            }
        }
        io.send(msg(Body::WriteStart(WriteStart {
            have,
            have_blake3: if have > 0 {
                hasher.clone().finalize().as_bytes().to_vec().into()
            } else {
                Vec::new().into()
            },
        })))
        .await?;
        let Some(FileMessage {
            body: Some(Body::UploadStart(up)),
        }) = io.recv().await?
        else {
            return Err(FileError::Protocol("expected UploadStart".into()));
        };
        if up.start != 0 && up.start != have {
            return Err(FileError::Protocol("resume offset does not match".into()));
        }
        if up.start == 0 {
            hasher = blake3::Hasher::new();
        }
        let mut out = open_part(&part, up.start).await?;
        let mut got = up.start;
        let promised = loop {
            let Some(m) = io.recv().await? else {
                // Stream ended mid-upload: keep the partial file so the upload can resume.
                out.flush().await.ok();
                return Err(FileError::Protocol("upload interrupted".into()));
            };
            match m.body {
                Some(Body::Chunk(c)) => {
                    got += c.data.len() as u64;
                    if got > r.size {
                        drop(out);
                        let _ = fs::remove_file(&part).await;
                        return Err(FileError::TooLarge("more data than announced".into()));
                    }
                    hasher.update(&c.data);
                    out.write_all(&c.data)
                        .await
                        .map_err(|e| FileError::from_io(&e, &r.path))?;
                }
                Some(Body::UploadEnd(end)) => break end.blake3,
                _ => return Err(FileError::Protocol("unexpected message in upload".into())),
            }
        };
        let digest = hasher.finalize();
        if got != r.size || !hash_matches(&digest, &promised) {
            drop(out);
            let _ = fs::remove_file(&part).await;
            warn!(path = %r.path, "upload failed verification; partial file discarded");
            return Err(FileError::HashMismatch(r.path));
        }
        out.flush()
            .await
            .map_err(|e| FileError::from_io(&e, &r.path))?;
        out.sync_all()
            .await
            .map_err(|e| FileError::from_io(&e, &r.path))?;
        if r.mtime_unix_ms > 0 {
            let when = std::time::UNIX_EPOCH + std::time::Duration::from_millis(r.mtime_unix_ms);
            let std_file = out.into_std().await;
            let _ = std_file.set_modified(when);
        } else {
            drop(out);
        }
        fs::rename(&part, &final_path)
            .await
            .map_err(|e| FileError::from_io(&e, &r.path))?;
        io.send(msg(Body::Done(FileDone {
            size: got,
            blake3: digest.as_bytes().to_vec().into(),
        })))
        .await
    }
}

/// Opens (creating) the partial file positioned at `at`, truncating anything beyond it.
async fn open_part(part: &Path, at: u64) -> Result<File, FileError> {
    let mut f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(part)
        .await
        .map_err(|e| FileError::from_io(&e, &part.display().to_string()))?;
    f.set_len(at)
        .await
        .map_err(|e| FileError::from_io(&e, "truncate"))?;
    f.seek(std::io::SeekFrom::Start(at))
        .await
        .map_err(|e| FileError::from_io(&e, "seek"))?;
    Ok(f)
}

async fn entry_of(path: &PathBuf, name: String) -> Option<FileEntry> {
    let link = fs::symlink_metadata(path).await.ok()?;
    let is_symlink = link.file_type().is_symlink();
    // Follow the link for size/type when it resolves; a dangling link is shown as a link.
    let meta = if is_symlink {
        fs::metadata(path).await.unwrap_or(link)
    } else {
        link
    };
    Some(FileEntry {
        name,
        is_dir: meta.is_dir(),
        is_symlink,
        size: if meta.is_dir() { 0 } else { meta.len() },
        mtime_unix_ms: unix_ms(meta.modified()),
        readonly: meta.permissions().readonly(),
    })
}
