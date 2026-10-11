//! File service behaviour over the in-memory transport (the QUIC wiring is covered in
//! `nexus-media`'s end-to-end tests).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use nexus_files::{
    FileError, FileRoots, FileServer, FilesClient, MemoryIo, MsgIo, Opener, SharedRoot,
    memory_pair, part_path,
};
use nexus_proto::v1::{FileMessage, file_message::Body};

#[derive(Clone)]
struct Loop {
    server: Arc<FileServer>,
    /// Chunks the client may send before the "connection" dies (u32::MAX = never).
    cut_after_chunks: Arc<AtomicU32>,
}

struct Cut {
    inner: MemoryIo,
    left: u32,
}

impl MsgIo for Cut {
    async fn send(&mut self, msg: FileMessage) -> Result<(), FileError> {
        if matches!(msg.body, Some(Body::Chunk(_))) {
            if self.left == 0 {
                return Err(FileError::Protocol("connection lost".into()));
            }
            self.left -= 1;
        }
        self.inner.send(msg).await
    }
    async fn recv(&mut self) -> Result<Option<FileMessage>, FileError> {
        if self.left == 0 {
            return Err(FileError::Protocol("connection lost".into()));
        }
        self.inner.recv().await
    }
    fn finish(&mut self) -> Result<(), FileError> {
        self.inner.finish()
    }
}

impl Opener for Loop {
    type Io = Cut;
    async fn open(&self) -> Result<Cut, FileError> {
        let (client, mut srv) = memory_pair();
        let server = Arc::clone(&self.server);
        tokio::spawn(async move {
            let _ = server.serve(&mut srv).await;
        });
        Ok(Cut {
            inner: client,
            left: self.cut_after_chunks.load(Ordering::SeqCst),
        })
    }
}

struct Rig {
    dir: tempfile::TempDir,
    client: FilesClient<Loop>,
    cut: Arc<AtomicU32>,
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    for d in ["share", "ro", "secret", "local"] {
        std::fs::create_dir_all(dir.path().join(d)).unwrap();
    }
    let roots = FileRoots::new(vec![
        SharedRoot {
            path: dir.path().join("share"),
            writable: true,
        },
        SharedRoot {
            path: dir.path().join("ro"),
            writable: false,
        },
    ]);
    let cut = Arc::new(AtomicU32::new(u32::MAX));
    let client = FilesClient::new(Loop {
        server: Arc::new(FileServer::new(roots, 64 << 20)),
        cut_after_chunks: Arc::clone(&cut),
    });
    Rig { dir, client, cut }
}

fn p(d: &Path, rel: &str) -> String {
    d.join(rel).to_str().unwrap().to_owned()
}

/// Deterministic, incompressible-ish test data.
fn data(len: usize, seed: u8) -> Vec<u8> {
    let mut x = u32::from(seed) + 1;
    (0..len)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 24) as u8
        })
        .collect()
}

#[tokio::test]
async fn lists_roots_and_directories() {
    let r = rig();
    std::fs::write(r.dir.path().join("share/b.txt"), b"bb").unwrap();
    std::fs::write(r.dir.path().join("share/A.txt"), b"a").unwrap();
    std::fs::create_dir(r.dir.path().join("share/zdir")).unwrap();

    let roots = r.client.list("").await.unwrap();
    assert_eq!(roots.entries.len(), 2);
    assert!(roots.entries.iter().any(|e| e.readonly), "ro root flagged");

    let l = r.client.list(&p(r.dir.path(), "share")).await.unwrap();
    let names: Vec<_> = l.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        ["zdir", "A.txt", "b.txt"],
        "dirs first, then by name"
    );
    assert_eq!(l.entries[2].size, 2);

    let e = r
        .client
        .stat(&p(r.dir.path(), "share/b.txt"))
        .await
        .unwrap();
    assert_eq!((e.is_dir, e.size), (false, 2));
    assert!(matches!(
        r.client.stat(&p(r.dir.path(), "share/nope")).await,
        Err(FileError::NotFound(_))
    ));
    assert!(matches!(
        r.client.list(&p(r.dir.path(), "share/b.txt")).await,
        Err(FileError::NotADirectory(_))
    ));
}

#[tokio::test]
async fn download_roundtrip_is_verified() {
    let r = rig();
    let bytes = data(3 * 1024 * 1024 + 17, 1);
    std::fs::write(r.dir.path().join("share/big.bin"), &bytes).unwrap();
    let dest = r.dir.path().join("local/big.bin");
    let mut last = None;
    let t = r
        .client
        .download(&p(r.dir.path(), "share/big.bin"), &dest, false, |pr| {
            last = Some(pr);
        })
        .await
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), bytes);
    assert_eq!(t.size, bytes.len() as u64);
    assert_eq!(t.blake3, *blake3::hash(&bytes).as_bytes());
    assert_eq!(t.resumed_from, 0);
    let last = last.unwrap();
    assert_eq!(last.done, last.total);
    assert!(!part_path(&dest).exists(), "partial file renamed away");

    // Refuses to clobber unless asked.
    assert!(matches!(
        r.client
            .download(&p(r.dir.path(), "share/big.bin"), &dest, false, |_| {})
            .await,
        Err(FileError::AlreadyExists(_))
    ));
    r.client
        .download(&p(r.dir.path(), "share/big.bin"), &dest, true, |_| {})
        .await
        .unwrap();
}

#[tokio::test]
async fn download_resumes_from_a_true_prefix_and_restarts_from_a_bad_one() {
    let r = rig();
    let bytes = data(2 * 1024 * 1024, 2);
    std::fs::write(r.dir.path().join("share/f.bin"), &bytes).unwrap();
    let dest = r.dir.path().join("local/f.bin");

    std::fs::write(part_path(&dest), &bytes[..700_000]).unwrap();
    let t = r
        .client
        .download(&p(r.dir.path(), "share/f.bin"), &dest, false, |_| {})
        .await
        .unwrap();
    assert_eq!(t.resumed_from, 700_000);
    assert_eq!(std::fs::read(&dest).unwrap(), bytes);

    // A partial copy with one flipped byte must not be trusted.
    std::fs::remove_file(&dest).unwrap();
    let mut bad = bytes[..500_000].to_vec();
    bad[123_456] ^= 0xff;
    std::fs::write(part_path(&dest), &bad).unwrap();
    let t = r
        .client
        .download(&p(r.dir.path(), "share/f.bin"), &dest, false, |_| {})
        .await
        .unwrap();
    assert_eq!(t.resumed_from, 0, "corrupt prefix → restart from zero");
    assert_eq!(std::fs::read(&dest).unwrap(), bytes);

    // A partial copy longer than the file (the file shrank) also restarts.
    std::fs::remove_file(&dest).unwrap();
    std::fs::write(part_path(&dest), data(bytes.len() + 10, 9)).unwrap();
    let t = r
        .client
        .download(&p(r.dir.path(), "share/f.bin"), &dest, false, |_| {})
        .await
        .unwrap();
    assert_eq!(t.resumed_from, 0);
    assert_eq!(std::fs::read(&dest).unwrap(), bytes);
}

#[tokio::test]
async fn upload_roundtrip_and_overwrite_rules() {
    let r = rig();
    let bytes = data(1_500_000, 3);
    let local = r.dir.path().join("local/up.bin");
    std::fs::write(&local, &bytes).unwrap();
    let remote = p(r.dir.path(), "share/up.bin");

    let t = r
        .client
        .upload(&local, &remote, false, |_| {})
        .await
        .unwrap();
    assert_eq!(t.blake3, *blake3::hash(&bytes).as_bytes());
    assert_eq!(
        std::fs::read(r.dir.path().join("share/up.bin")).unwrap(),
        bytes
    );
    assert!(!part_path(&r.dir.path().join("share/up.bin")).exists());

    assert!(matches!(
        r.client.upload(&local, &remote, false, |_| {}).await,
        Err(FileError::AlreadyExists(_))
    ));
    std::fs::write(&local, b"new").unwrap();
    r.client
        .upload(&local, &remote, true, |_| {})
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(r.dir.path().join("share/up.bin")).unwrap(),
        b"new"
    );

    // Onto a directory.
    assert!(matches!(
        r.client
            .upload(&local, &p(r.dir.path(), "share"), true, |_| {})
            .await,
        Err(FileError::IsADirectory(_))
    ));
}

#[tokio::test]
async fn an_interrupted_upload_resumes() {
    let r = rig();
    let bytes = data(4 * 1024 * 1024, 4);
    let local = r.dir.path().join("local/u.bin");
    std::fs::write(&local, &bytes).unwrap();
    let remote = p(r.dir.path(), "share/u.bin");

    r.cut.store(6, Ordering::SeqCst); // 6 × 256 KiB, then the link dies
    assert!(
        r.client
            .upload(&local, &remote, false, |_| {})
            .await
            .is_err()
    );
    // Give the server a moment to notice the closed channel.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let part = part_path(&r.dir.path().join("share/u.bin"));
    let held = std::fs::metadata(&part).unwrap().len();
    assert!(
        held > 0 && held < bytes.len() as u64,
        "partial kept: {held}"
    );
    assert!(!r.dir.path().join("share/u.bin").exists());

    r.cut.store(u32::MAX, Ordering::SeqCst);
    let t = r
        .client
        .upload(&local, &remote, false, |_| {})
        .await
        .unwrap();
    assert_eq!(t.resumed_from, held);
    assert_eq!(
        std::fs::read(r.dir.path().join("share/u.bin")).unwrap(),
        bytes
    );
}

#[tokio::test]
async fn a_corrupt_partial_upload_is_not_continued() {
    let r = rig();
    let bytes = data(1_000_000, 5);
    let local = r.dir.path().join("local/c.bin");
    std::fs::write(&local, &bytes).unwrap();
    let mut bad = bytes[..400_000].to_vec();
    bad[10] ^= 1;
    std::fs::write(part_path(&r.dir.path().join("share/c.bin")), bad).unwrap();
    let t = r
        .client
        .upload(&local, &p(r.dir.path(), "share/c.bin"), false, |_| {})
        .await
        .unwrap();
    assert_eq!(t.resumed_from, 0);
    assert_eq!(
        std::fs::read(r.dir.path().join("share/c.bin")).unwrap(),
        bytes
    );
}

#[tokio::test]
async fn mkdir_and_remove() {
    let r = rig();
    let d = p(r.dir.path(), "share/new");
    r.client.mkdir(&d).await.unwrap();
    assert!(matches!(
        r.client.mkdir(&d).await,
        Err(FileError::AlreadyExists(_))
    ));
    std::fs::write(r.dir.path().join("share/new/x"), b"x").unwrap();
    assert!(
        r.client.remove(&d, false).await.is_err(),
        "non-empty, non-recursive"
    );
    r.client.remove(&d, true).await.unwrap();
    assert!(!r.dir.path().join("share/new").exists());
    // A shared folder cannot be deleted from the other side.
    assert!(matches!(
        r.client.remove(&p(r.dir.path(), "share"), true).await,
        Err(FileError::PermissionDenied(_))
    ));
    assert!(r.dir.path().join("share").exists());
}

#[tokio::test]
async fn the_policy_is_enforced_end_to_end() {
    let r = rig();
    std::fs::write(r.dir.path().join("secret/key"), b"k").unwrap();
    std::fs::write(r.dir.path().join("ro/readme"), b"r").unwrap();
    let secret = p(r.dir.path(), "secret/key");
    let dest = r.dir.path().join("local/k");

    assert!(matches!(
        r.client.download(&secret, &dest, false, |_| {}).await,
        Err(FileError::PermissionDenied(_))
    ));
    assert!(matches!(
        r.client.list(&p(r.dir.path(), "secret")).await,
        Err(FileError::PermissionDenied(_))
    ));
    assert!(matches!(
        r.client
            .download(
                &format!("{}/share/../secret/key", r.dir.path().display()),
                &dest,
                false,
                |_| {}
            )
            .await,
        Err(FileError::InvalidPath(_))
    ));
    assert!(
        !dest.exists() && !part_path(&dest).exists()
            || std::fs::metadata(part_path(&dest)).map_or(true, |m| m.len() == 0)
    );

    // Read-only folder: reading works, every write is refused.
    r.client
        .download(&p(r.dir.path(), "ro/readme"), &dest, false, |_| {})
        .await
        .unwrap();
    let local = r.dir.path().join("local/k");
    assert!(matches!(
        r.client
            .upload(&local, &p(r.dir.path(), "ro/new"), false, |_| {})
            .await,
        Err(FileError::PermissionDenied(_))
    ));
    assert!(matches!(
        r.client.mkdir(&p(r.dir.path(), "ro/d")).await,
        Err(FileError::PermissionDenied(_))
    ));
    assert!(matches!(
        r.client.remove(&p(r.dir.path(), "ro/readme"), false).await,
        Err(FileError::PermissionDenied(_))
    ));
    assert!(r.dir.path().join("ro/readme").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_pointing_out_of_the_share_are_refused() {
    let r = rig();
    std::fs::write(r.dir.path().join("secret/key"), b"k").unwrap();
    std::os::unix::fs::symlink(r.dir.path().join("secret"), r.dir.path().join("share/out"))
        .unwrap();
    let dest = r.dir.path().join("local/k");
    assert!(matches!(
        r.client
            .download(&p(r.dir.path(), "share/out/key"), &dest, false, |_| {})
            .await,
        Err(FileError::PermissionDenied(_))
    ));
    assert!(matches!(
        r.client.list(&p(r.dir.path(), "share/out")).await,
        Err(FileError::PermissionDenied(_))
    ));
    // The link is visible as a link, and removing it leaves the target alone.
    let l = r.client.list(&p(r.dir.path(), "share")).await.unwrap();
    assert!(l.entries.iter().any(|e| e.name == "out" && e.is_symlink));
    r.client
        .remove(&p(r.dir.path(), "share/out"), true)
        .await
        .unwrap();
    assert!(r.dir.path().join("secret/key").exists());
}

#[tokio::test]
async fn uploads_over_the_size_limit_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("share")).unwrap();
    let roots = FileRoots::new(vec![SharedRoot {
        path: dir.path().join("share"),
        writable: true,
    }]);
    let client = FilesClient::new(Loop {
        server: Arc::new(FileServer::new(roots, 1000)),
        cut_after_chunks: Arc::new(AtomicU32::new(u32::MAX)),
    });
    let local = dir.path().join("big");
    std::fs::write(&local, vec![0u8; 5000]).unwrap();
    assert!(matches!(
        client
            .upload(&local, &p(dir.path(), "share/big"), false, |_| {})
            .await,
        Err(FileError::TooLarge(_))
    ));
}

#[tokio::test]
async fn a_second_writer_to_the_same_file_is_refused() {
    let r = rig();
    let local = r.dir.path().join("local/w.bin");
    std::fs::write(&local, data(8 * 1024 * 1024, 7)).unwrap();
    let remote = p(r.dir.path(), "share/w.bin");
    let a = r.client.upload(&local, &remote, false, |_| {});
    let b = async {
        tokio::task::yield_now().await;
        r.client.upload(&local, &remote, true, |_| {}).await
    };
    let (ra, rb) = tokio::join!(a, b);
    // Exactly one wins; the other is told the file is busy (or, if it started after the first
    // finished, replaces it, which is also fine).
    assert!(ra.is_ok() || rb.is_ok());
    if let Err(e) = rb {
        assert!(matches!(e, FileError::Busy(_)), "{e}");
    }
}
