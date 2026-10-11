//! Creating, loading and deleting the device identity in a [`SecretStore`].

use crate::error::CryptoError;
use crate::identity::{DeviceIdentity, IdentitySeed};
use crate::store::SecretStore;

/// Name under which the root seed is stored.
pub const IDENTITY_SECRET_NAME: &str = "device-identity-seed";

/// Whether [`load_or_create_identity`] made a new identity or found one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provisioned {
    /// A new identity was generated and persisted.
    Created,
    /// An existing identity was loaded.
    Loaded,
}

/// Loads the identity if one exists.
pub fn load_identity(store: &dyn SecretStore) -> Result<Option<DeviceIdentity>, CryptoError> {
    match store.load(IDENTITY_SECRET_NAME)? {
        Some(raw) => {
            let seed = IdentitySeed::from_slice(&raw)?;
            Ok(Some(DeviceIdentity::from_seed(&seed)))
        }
        None => Ok(None),
    }
}

/// Generates and persists a new identity. Fails with [`CryptoError::AlreadyExists`] rather than
/// overwrite: replacing an identity orphans every pairing, so it must be an explicit act
/// (`delete_identity` first).
pub fn create_identity(store: &dyn SecretStore) -> Result<DeviceIdentity, CryptoError> {
    let seed = IdentitySeed::generate()?;
    if !store.store_if_absent(IDENTITY_SECRET_NAME, seed.as_bytes())? {
        return Err(CryptoError::AlreadyExists);
    }
    Ok(DeviceIdentity::from_seed(&seed))
}

/// Loads the identity, creating it on first run. Safe against two processes starting at once:
/// the loser of the creation race loads the winner's identity.
pub fn load_or_create_identity(
    store: &dyn SecretStore,
) -> Result<(DeviceIdentity, Provisioned), CryptoError> {
    // Two attempts are enough: a second failure to create means the file appeared in between,
    // so the next load must succeed.
    for _ in 0..3 {
        if let Some(identity) = load_identity(store)? {
            return Ok((identity, Provisioned::Loaded));
        }
        match create_identity(store) {
            Ok(identity) => return Ok((identity, Provisioned::Created)),
            Err(CryptoError::AlreadyExists) => {}
            Err(other) => return Err(other),
        }
    }
    Err(CryptoError::Invalid(
        "identity could not be created or loaded",
    ))
}

/// Permanently removes the identity. Returns whether one existed.
pub fn delete_identity(store: &dyn SecretStore) -> Result<bool, CryptoError> {
    Ok(store.delete(IDENTITY_SECRET_NAME)?)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::store::{FileStore, MemoryStore};

    #[test]
    fn first_run_creates_then_loads_same_identity() {
        let store = MemoryStore::new();
        let (first, how) = load_or_create_identity(&store).unwrap();
        assert_eq!(how, Provisioned::Created);
        let (second, how) = load_or_create_identity(&store).unwrap();
        assert_eq!(how, Provisioned::Loaded);
        assert_eq!(first.public(), second.public());
    }

    #[test]
    fn create_refuses_to_overwrite() {
        let store = MemoryStore::new();
        let original = create_identity(&store).unwrap();
        assert!(matches!(
            create_identity(&store),
            Err(CryptoError::AlreadyExists)
        ));
        assert_eq!(
            load_identity(&store).unwrap().unwrap().public(),
            original.public()
        );
    }

    #[test]
    fn delete_then_recreate_gives_a_new_identity() {
        let store = MemoryStore::new();
        let a = create_identity(&store).unwrap();
        assert!(delete_identity(&store).unwrap());
        assert!(!delete_identity(&store).unwrap());
        assert!(load_identity(&store).unwrap().is_none());
        let b = create_identity(&store).unwrap();
        assert_ne!(a.device_id(), b.device_id());
    }

    #[test]
    fn wrong_size_seed_in_store_is_an_error_not_a_panic() {
        let store = MemoryStore::new();
        store.store(IDENTITY_SECRET_NAME, &[1, 2, 3]).unwrap();
        assert!(matches!(
            load_identity(&store),
            Err(CryptoError::Invalid(_))
        ));
    }

    #[test]
    fn concurrent_first_runs_agree_on_one_identity() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FileStore::new(dir.path().join("keys")));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let store = store.clone();
                std::thread::spawn(move || {
                    load_or_create_identity(store.as_ref())
                        .unwrap()
                        .0
                        .device_id()
                })
            })
            .collect();
        let ids: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(
            ids.windows(2).all(|w| w[0] == w[1]),
            "processes disagreed: {ids:?}"
        );
    }
}
