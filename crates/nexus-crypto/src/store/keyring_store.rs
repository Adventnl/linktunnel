use keyring::Entry;
use zeroize::Zeroizing;

use super::{SecretStore, StoreKind, validate_name};
use crate::error::StoreError;

/// The operating system's credential store via the `keyring` crate:
/// macOS Keychain, Windows Credential Manager (DPAPI), Linux Secret Service (GNOME Keyring,
/// KWallet).
#[derive(Debug, Clone)]
pub struct KeyringStore {
    service: String,
}

impl KeyringStore {
    /// Namespaces all items under `service` (e.g. `app.nexus-remote`).
    #[must_use]
    pub fn new(service: &str) -> Self {
        Self {
            service: service.to_owned(),
        }
    }

    /// Checks the credential store is reachable without creating anything.
    ///
    /// On a headless Linux box with no Secret Service daemon this fails, which is the signal to
    /// fall back to the file backend.
    pub fn probe(&self) -> Result<(), StoreError> {
        if let Err(e) = Entry::store_status() {
            return Err(StoreError::Unavailable(e.to_string()));
        }
        match self.entry("probe.nonexistent")?.get_secret() {
            Ok(_) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(map_error(e)),
        }
    }

    fn entry(&self, name: &str) -> Result<Entry, StoreError> {
        validate_name(name)?;
        Entry::new(&self.service, name).map_err(map_error)
    }
}

fn map_error(e: keyring::Error) -> StoreError {
    match e {
        keyring::Error::NoDefaultStore
        | keyring::Error::NoStorageAccess(_)
        | keyring::Error::PlatformFailure(_) => StoreError::Unavailable(e.to_string()),
        other => StoreError::Backend(other.to_string()),
    }
}

impl SecretStore for KeyringStore {
    fn kind(&self) -> StoreKind {
        StoreKind::OsKeychain
    }

    fn describe(&self) -> String {
        if cfg!(target_os = "macos") {
            "macOS Keychain".into()
        } else if cfg!(target_os = "windows") {
            "Windows Credential Manager".into()
        } else {
            "Secret Service".into()
        }
    }

    fn load(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>, StoreError> {
        match self.entry(name)?.get_secret() {
            Ok(secret) => Ok(Some(Zeroizing::new(secret))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_error(e)),
        }
    }

    fn store(&self, name: &str, secret: &[u8]) -> Result<(), StoreError> {
        self.entry(name)?.set_secret(secret).map_err(map_error)
    }

    fn delete(&self, name: &str) -> Result<bool, StoreError> {
        match self.entry(name)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(map_error(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Touches the real OS credential store, which may show an authorisation prompt on a
    /// workstation and does not exist on CI runners. Run explicitly:
    /// `cargo test -p nexus-crypto -- --ignored keyring`.
    #[test]
    #[ignore = "uses the real OS credential store"]
    fn keyring_roundtrip_against_real_store() {
        let mut nonce = [0u8; 6];
        getrandom::fill(&mut nonce).unwrap();
        let service = format!(
            "nexus-remote-test-{}",
            data_encoding::HEXLOWER.encode(&nonce)
        );
        let store = KeyringStore::new(&service);
        store.probe().unwrap();

        assert!(store.load("device-identity-seed").unwrap().is_none());
        store.store("device-identity-seed", &[0xAB; 32]).unwrap();
        assert_eq!(
            &**store.load("device-identity-seed").unwrap().unwrap(),
            &[0xAB; 32]
        );
        store.store("device-identity-seed", &[0xCD; 32]).unwrap();
        assert_eq!(
            &**store.load("device-identity-seed").unwrap().unwrap(),
            &[0xCD; 32]
        );
        assert!(store.delete("device-identity-seed").unwrap());
        assert!(!store.delete("device-identity-seed").unwrap());
        assert!(store.load("device-identity-seed").unwrap().is_none());
    }

    #[test]
    fn invalid_names_never_reach_the_os() {
        let store = KeyringStore::new("nexus-remote-test");
        assert!(store.load("../escape").is_err());
        assert!(store.store("UPPER", b"x").is_err());
    }
}
