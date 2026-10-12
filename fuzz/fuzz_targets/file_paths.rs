//! The file panel's sandbox: no string a peer can send may resolve to a path outside the shared
//! folder, whatever it contains (`..`, absolute paths, odd encodings).
#![no_main]

use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use nexus_files::{Access, FileRoots, SharedRoot};

struct World {
    roots: FileRoots,
    root: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

fn world() -> &'static World {
    static W: OnceLock<World> = OnceLock::new();
    W.get_or_init(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("share").canonicalize().unwrap_or_else(|_| {
            std::fs::create_dir_all(dir.path().join("share")).expect("mkdir");
            dir.path().join("share").canonicalize().expect("canonical")
        });
        std::fs::create_dir_all(root.join("sub")).expect("mkdir");
        std::fs::write(root.join("sub/file.txt"), b"x").expect("write");
        // A symlink pointing outside: must never be followed out of the sandbox.
        #[cfg(unix)]
        let _ = std::os::unix::fs::symlink("/etc", root.join("escape"));
        World { roots: FileRoots::new(vec![SharedRoot { path: root.clone(), writable: true }]), root, _dir: dir }
    })
}

fuzz_target!(|data: &[u8]| {
    let Ok(wire) = std::str::from_utf8(data) else { return };
    let w = world();
    // Anchor half of the inputs inside the root, since a bare random string is almost always
    // "outside" and so tests nothing.
    let anchored = format!("{}/{}", w.root.display(), wire);
    for candidate in [wire, anchored.as_str()] {
        for access in [Access::Read, Access::Write] {
            for resolved in [
                w.roots.resolve(candidate, access),
                w.roots.resolve_new(candidate, access),
                w.roots.resolve_entry(candidate, access),
            ]
            .into_iter()
            .flatten()
            {
                assert!(resolved.starts_with(&w.root), "{candidate:?} escaped to {resolved:?}");
            }
        }
    }
});
