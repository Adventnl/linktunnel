use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use zeroize::Zeroizing;

use super::{SecretStore, StoreKind, validate_name};
use crate::error::StoreError;

const MAGIC: &[u8; 8] = b"NXSECRET";
const FORMAT_VERSION: u8 = 1;
const HEADER_LEN: usize = MAGIC.len() + 1 + 2;
const CHECK_LEN: usize = 16;
/// No Nexus secret is anywhere near this large; anything bigger is corruption or an attack.
const MAX_SECRET_LEN: usize = 4096;

/// Stores each secret in its own owner-only file inside one directory.
///
/// * Files are created `0600` (Unix) via a temp file + `fsync` + atomic `rename`, so a crash can
///   never leave a half-written key.
/// * On load, a file that is group/world accessible or owned by another user is **refused**
///   rather than silently used: a key that someone else could have read or planted is not a key.
/// * A BLAKE3 checksum detects truncation and bit-rot (it is not a security measure).
///
/// On Windows the file inherits the ACL of its directory; the installer must create the data
/// directory readable only by `SYSTEM` and Administrators. (A DPAPI-wrapped variant is planned
/// for the Windows service in Phase 6.)
#[derive(Debug, Clone)]
pub struct FileStore {
    dir: PathBuf,
}

impl FileStore {
    /// Uses `dir` for storage; it is created (`0700`) on first write.
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.secret"))
    }

    fn encode(secret: &[u8]) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        if secret.len() > MAX_SECRET_LEN {
            return Err(StoreError::Backend("secret too large".into()));
        }
        let mut out = Zeroizing::new(Vec::with_capacity(HEADER_LEN + secret.len() + CHECK_LEN));
        out.extend_from_slice(MAGIC);
        out.push(FORMAT_VERSION);
        let len = u16::try_from(secret.len())
            .map_err(|_| StoreError::Backend("secret too large".into()))?;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(secret);
        let check = blake3::hash(&out);
        out.extend_from_slice(&check.as_bytes()[..CHECK_LEN]);
        Ok(out)
    }

    fn decode(name: &str, raw: &[u8]) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        let corrupt = |reason| StoreError::Corrupt {
            name: name.to_owned(),
            reason,
        };
        if raw.len() < HEADER_LEN + CHECK_LEN || &raw[..MAGIC.len()] != MAGIC {
            return Err(corrupt("bad magic or truncated header"));
        }
        if raw[MAGIC.len()] != FORMAT_VERSION {
            return Err(corrupt("unsupported file format version"));
        }
        let len = usize::from(u16::from_le_bytes([
            raw[MAGIC.len() + 1],
            raw[MAGIC.len() + 2],
        ]));
        if raw.len() != HEADER_LEN + len + CHECK_LEN {
            return Err(corrupt("length does not match file size"));
        }
        let (body, check) = raw.split_at(HEADER_LEN + len);
        let expected = blake3::hash(body);
        if expected.as_bytes()[..CHECK_LEN] != *check {
            return Err(corrupt("checksum mismatch"));
        }
        Ok(Zeroizing::new(body[HEADER_LEN..].to_vec()))
    }

    fn ensure_dir(&self) -> Result<(), StoreError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&self.dir)?;
        }
        #[cfg(not(unix))]
        fs::create_dir_all(&self.dir)?;
        Ok(())
    }

    /// Writes `contents` to a fresh private temp file next to the final path.
    fn write_temp(&self, name: &str, contents: &[u8]) -> Result<PathBuf, StoreError> {
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).map_err(|e| StoreError::Backend(e.to_string()))?;
        let tmp = self.dir.join(format!(
            ".{name}.{}.tmp",
            data_encoding::HEXLOWER.encode(&nonce)
        ));

        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp)?;
        let result = file.write_all(contents).and_then(|()| file.sync_all());
        if let Err(e) = result {
            let _ = fs::remove_file(&tmp);
            return Err(e.into());
        }
        Ok(tmp)
    }

    fn sync_dir(&self) {
        // Persisting the rename is best-effort; failure only risks losing the *latest* write.
        if let Ok(dir) = File::open(&self.dir) {
            let _ = dir.sync_all();
        }
    }

    /// Refuses key files that are accessible to others or owned by another user.
    #[cfg(unix)]
    fn check_permissions(path: &Path, file: &File) -> Result<(), StoreError> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = file.metadata()?;
        let display = path.display().to_string();
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(StoreError::InsecurePermissions {
                path: display,
                reason: "accessible by group or others (expected mode 0600)",
            });
        }
        if meta.uid() != rustix::process::geteuid().as_raw() {
            return Err(StoreError::InsecurePermissions {
                path: display,
                reason: "owned by a different user",
            });
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn check_permissions(_path: &Path, _file: &File) -> Result<(), StoreError> {
        Ok(())
    }
}

impl SecretStore for FileStore {
    fn kind(&self) -> StoreKind {
        StoreKind::File
    }

    fn describe(&self) -> String {
        self.dir.display().to_string()
    }

    fn load(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>, StoreError> {
        validate_name(name)?;
        let path = self.path(name);
        let mut file = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        Self::check_permissions(&path, &file)?;

        let mut raw = Zeroizing::new(Vec::new());
        // Read at most one byte past the cap so a huge/hostile file cannot exhaust memory.
        let cap = (HEADER_LEN + MAX_SECRET_LEN + CHECK_LEN + 1) as u64;
        (&mut file).take(cap).read_to_end(&mut raw)?;
        Self::decode(name, &raw).map(Some)
    }

    fn store(&self, name: &str, secret: &[u8]) -> Result<(), StoreError> {
        validate_name(name)?;
        self.ensure_dir()?;
        let encoded = Self::encode(secret)?;
        let tmp = self.write_temp(name, &encoded)?;
        if let Err(e) = fs::rename(&tmp, self.path(name)) {
            let _ = fs::remove_file(&tmp);
            return Err(e.into());
        }
        self.sync_dir();
        Ok(())
    }

    fn store_if_absent(&self, name: &str, secret: &[u8]) -> Result<bool, StoreError> {
        validate_name(name)?;
        self.ensure_dir()?;
        let encoded = Self::encode(secret)?;
        let tmp = self.write_temp(name, &encoded)?;
        // `hard_link` fails with AlreadyExists if the target exists: an atomic create-if-absent
        // that two racing processes cannot both win.
        let linked = fs::hard_link(&tmp, self.path(name));
        let _ = fs::remove_file(&tmp);
        match linked {
            Ok(()) => {
                self.sync_dir();
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    fn delete(&self, name: &str) -> Result<bool, StoreError> {
        validate_name(name)?;
        match fs::remove_file(self.path(name)) {
            Ok(()) => {
                self.sync_dir();
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, FileStore) {
        let dir = tempfile::tempdir().unwrap();
        let s = FileStore::new(dir.path().join("secrets"));
        (dir, s)
    }

    #[test]
    fn roundtrip_overwrite_delete() {
        let (_d, s) = store();
        assert!(s.load("seed").unwrap().is_none());
        s.store("seed", &[7; 32]).unwrap();
        assert_eq!(&**s.load("seed").unwrap().unwrap(), &[7; 32]);
        s.store("seed", &[8; 32]).unwrap();
        assert_eq!(&**s.load("seed").unwrap().unwrap(), &[8; 32]);
        assert!(s.delete("seed").unwrap());
        assert!(!s.delete("seed").unwrap());
        assert!(s.load("seed").unwrap().is_none());
    }

    #[test]
    fn empty_and_max_size_secrets_roundtrip() {
        let (_d, s) = store();
        s.store("empty", &[]).unwrap();
        assert!(s.load("empty").unwrap().unwrap().is_empty());
        s.store("big", &vec![1u8; MAX_SECRET_LEN]).unwrap();
        assert_eq!(s.load("big").unwrap().unwrap().len(), MAX_SECRET_LEN);
        assert!(s.store("too-big", &vec![1u8; MAX_SECRET_LEN + 1]).is_err());
    }

    #[test]
    fn store_if_absent_is_atomic_and_exclusive() {
        let (_d, s) = store();
        assert!(s.store_if_absent("seed", b"first").unwrap());
        assert!(!s.store_if_absent("seed", b"second").unwrap());
        assert_eq!(&**s.load("seed").unwrap().unwrap(), b"first");
    }

    #[test]
    fn racing_creators_produce_exactly_one_winner() {
        let (_d, s) = store();
        let s = std::sync::Arc::new(s);
        let handles: Vec<_> = (0..8u8)
            .map(|i| {
                let s = s.clone();
                std::thread::spawn(move || s.store_if_absent("seed", &[i; 32]).unwrap())
            })
            .collect();
        let winners = handles
            .into_iter()
            .filter_map(|h| h.join().ok())
            .filter(|w| *w)
            .count();
        assert_eq!(winners, 1);
        // And the surviving value is intact (one of the writers', not a mix).
        let v = s.load("seed").unwrap().unwrap();
        assert!(v.iter().all(|b| *b == v[0]) && v.len() == 32);
    }

    #[test]
    fn no_temp_files_are_left_behind() {
        let (_d, s) = store();
        s.store("seed", b"x").unwrap();
        s.store_if_absent("seed", b"y").unwrap();
        let names: Vec<_> = fs::read_dir(&s.dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["seed.secret".to_string()]);
    }

    #[test]
    fn corruption_is_detected() {
        let (_d, s) = store();
        s.store("seed", &[5; 32]).unwrap();
        let path = s.path("seed");
        let original = fs::read(&path).unwrap();

        for (label, mutate) in [
            ("flipped payload bit", {
                let mut v = original.clone();
                v[HEADER_LEN + 3] ^= 1;
                v
            }),
            ("truncated", original[..original.len() - 1].to_vec()),
            ("bad magic", {
                let mut v = original.clone();
                v[0] = b'X';
                v
            }),
            ("future version", {
                let mut v = original.clone();
                v[MAGIC.len()] = 99;
                v
            }),
            ("empty file", Vec::new()),
        ] {
            fs::write(&path, &mutate).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            }
            assert!(
                matches!(s.load("seed"), Err(StoreError::Corrupt { .. })),
                "{label} was not detected"
            );
        }
    }

    #[test]
    fn oversized_file_is_rejected_without_reading_it_all() {
        let (_d, s) = store();
        s.store("seed", b"x").unwrap();
        let path = s.path("seed");
        fs::write(&path, vec![0u8; 5 * 1024 * 1024]).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(matches!(s.load("seed"), Err(StoreError::Corrupt { .. })));
    }

    #[cfg(unix)]
    #[test]
    fn files_are_private_and_loose_permissions_are_refused() {
        use std::os::unix::fs::PermissionsExt;
        let (_d, s) = store();
        s.store("seed", &[1; 32]).unwrap();
        let file_mode = fs::metadata(s.path("seed")).unwrap().permissions().mode() & 0o777;
        let dir_mode = fs::metadata(&s.dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600);
        assert_eq!(dir_mode, 0o700);

        for loose in [0o640, 0o604, 0o644, 0o666, 0o660] {
            fs::set_permissions(s.path("seed"), fs::Permissions::from_mode(loose)).unwrap();
            assert!(
                matches!(s.load("seed"), Err(StoreError::InsecurePermissions { .. })),
                "mode {loose:o} was accepted"
            );
        }
        // Tightening the mode makes it usable again.
        fs::set_permissions(s.path("seed"), fs::Permissions::from_mode(0o600)).unwrap();
        assert!(s.load("seed").unwrap().is_some());
    }

    #[test]
    fn path_traversal_names_are_rejected() {
        let (_d, s) = store();
        for bad in ["../x", "a/b", "..", ""] {
            assert!(s.store(bad, b"x").is_err());
            assert!(s.load(bad).is_err());
            assert!(s.delete(bad).is_err());
        }
    }
}
