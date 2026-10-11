//! Where the device's root secret lives.
//!
//! | Backend | Protects against | Used when |
//! |---|---|---|
//! | OS keychain (`keyring`: macOS Keychain, Windows Credential Manager/DPAPI, Linux Secret Service) | other users, offline disk access (login-keyed) | interactive user scope |
//! | Owner-only file (`0600`, owner checked) | other local users | headless servers and system services, where no interactive keychain exists |
//! | Memory | nothing (tests only) | unit tests |
//!
//! The file backend is *not* weaker than a keychain against an attacker who is already root or
//! the same user; the OS keychain adds protection from offline disk access and from other
//! processes of the same user on platforms that scope keychain items per application. Hardware
//! binding (TPM / Secure Enclave) is tracked in the hardening backlog: Secure Enclave cannot
//! hold Ed25519 keys, and TPM 2.0 support for Ed25519 is not universal, so the portable design
//! stores a software seed and wraps it with hardware where available.

mod file;
mod keyring_store;
mod memory;

use std::fmt;
use std::path::PathBuf;

use zeroize::Zeroizing;

pub use self::file::FileStore;
pub use self::keyring_store::KeyringStore;
pub use self::memory::MemoryStore;
use crate::error::StoreError;

/// Longest allowed secret name.
const MAX_NAME_LEN: usize = 64;

/// Which backend a store is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreKind {
    /// The operating system's credential store.
    OsKeychain,
    /// A private file on disk.
    File,
    /// Process memory (tests).
    Memory,
}

/// A place to keep small named secrets.
///
/// Implementations are blocking (OS credential APIs are); call from `spawn_blocking` in async code.
pub trait SecretStore: Send + Sync + fmt::Debug {
    /// Backend kind.
    fn kind(&self) -> StoreKind;

    /// Human-readable location, e.g. `macOS Keychain` or `/var/lib/nexus`.
    fn describe(&self) -> String;

    /// Reads a secret. `Ok(None)` when it does not exist.
    fn load(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>, StoreError>;

    /// Creates or overwrites a secret.
    fn store(&self, name: &str, secret: &[u8]) -> Result<(), StoreError>;

    /// Creates a secret only if none exists; returns whether it was created.
    ///
    /// The default implementation is check-then-write and therefore racy; backends that can do
    /// better (the file store links atomically) override it.
    fn store_if_absent(&self, name: &str, secret: &[u8]) -> Result<bool, StoreError> {
        if self.load(name)?.is_some() {
            return Ok(false);
        }
        self.store(name, secret)?;
        Ok(true)
    }

    /// Removes a secret. Returns whether it existed.
    fn delete(&self, name: &str) -> Result<bool, StoreError>;
}

/// Names become file names and keychain account names, so keep them boring: lowercase ASCII,
/// digits, `.`, `_`, `-`; must start alphanumeric. This also makes path traversal impossible.
pub(crate) fn validate_name(name: &str) -> Result<(), StoreError> {
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(StoreError::Backend(format!("invalid secret name `{name}`")))
    }
}

/// Options for [`open_default`].
#[derive(Debug, Clone)]
pub struct DefaultStoreOptions {
    /// Keychain service name (namespaces our items among other apps').
    pub service: String,
    /// Directory for the file backend.
    pub file_dir: PathBuf,
    /// Try the OS keychain first. Set false for system services and headless servers.
    pub prefer_os_store: bool,
    /// Fall back to the file backend when the OS keychain is unavailable.
    pub allow_file_fallback: bool,
}

/// Opens the best available store according to `opts`.
pub fn open_default(opts: &DefaultStoreOptions) -> Result<Box<dyn SecretStore>, StoreError> {
    if opts.prefer_os_store {
        let keyring = KeyringStore::new(&opts.service);
        match keyring.probe() {
            Ok(()) => return Ok(Box::new(keyring)),
            Err(e) if opts.allow_file_fallback => {
                tracing::warn!(error = %e, "OS keychain unavailable; using owner-only key file");
            }
            Err(e) => return Err(e),
        }
    }
    Ok(Box::new(FileStore::new(opts.file_dir.clone())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_validation() {
        for ok in ["device-identity-seed", "a", "k1.v2_x", "0abc"] {
            validate_name(ok).unwrap();
        }
        for bad in [
            "",
            "../etc/passwd",
            "a/b",
            "A",
            "-x",
            ".hidden",
            "sp ace",
            &"a".repeat(65),
            "ü",
        ] {
            assert!(validate_name(bad).is_err(), "`{bad}` accepted");
        }
    }

    #[test]
    fn default_store_prefers_file_when_os_store_not_requested() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_default(&DefaultStoreOptions {
            service: "nexus-test".into(),
            file_dir: dir.path().to_path_buf(),
            prefer_os_store: false,
            allow_file_fallback: true,
        })
        .unwrap();
        assert_eq!(store.kind(), StoreKind::File);
    }
}
