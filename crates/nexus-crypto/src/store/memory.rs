use std::collections::HashMap;
use std::sync::Mutex;

use zeroize::Zeroizing;

use super::{SecretStore, StoreKind, validate_name};
use crate::error::StoreError;

/// In-memory store for tests. Secrets vanish with the process.
#[derive(Debug, Default)]
pub struct MemoryStore {
    secrets: Mutex<HashMap<String, Zeroizing<Vec<u8>>>>,
}

impl MemoryStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Zeroizing<Vec<u8>>>> {
        // A poisoned lock only means another test thread panicked mid-write; the map is still
        // structurally valid, so keep going rather than cascade the panic.
        self.secrets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl SecretStore for MemoryStore {
    fn kind(&self) -> StoreKind {
        StoreKind::Memory
    }

    fn describe(&self) -> String {
        "process memory".into()
    }

    fn load(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>, StoreError> {
        validate_name(name)?;
        Ok(self.lock().get(name).map(|s| Zeroizing::new(s.to_vec())))
    }

    fn store(&self, name: &str, secret: &[u8]) -> Result<(), StoreError> {
        validate_name(name)?;
        self.lock()
            .insert(name.to_owned(), Zeroizing::new(secret.to_vec()));
        Ok(())
    }

    fn store_if_absent(&self, name: &str, secret: &[u8]) -> Result<bool, StoreError> {
        validate_name(name)?;
        let mut map = self.lock();
        if map.contains_key(name) {
            return Ok(false);
        }
        map.insert(name.to_owned(), Zeroizing::new(secret.to_vec()));
        Ok(true)
    }

    fn delete(&self, name: &str) -> Result<bool, StoreError> {
        validate_name(name)?;
        Ok(self.lock().remove(name).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_overwrite_delete() {
        let s = MemoryStore::new();
        assert!(s.load("k").unwrap().is_none());
        s.store("k", b"one").unwrap();
        assert_eq!(&**s.load("k").unwrap().unwrap(), b"one");
        s.store("k", b"two").unwrap();
        assert_eq!(&**s.load("k").unwrap().unwrap(), b"two");
        assert!(s.delete("k").unwrap());
        assert!(!s.delete("k").unwrap());
    }

    #[test]
    fn store_if_absent_is_exclusive() {
        let s = MemoryStore::new();
        assert!(s.store_if_absent("k", b"first").unwrap());
        assert!(!s.store_if_absent("k", b"second").unwrap());
        assert_eq!(&**s.load("k").unwrap().unwrap(), b"first");
    }
}
