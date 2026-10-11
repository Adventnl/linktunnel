//! Per-device local policy: which paired devices may use this one without being asked.
//!
//! This is *this* device's decision about a peer, so it lives outside the signed pairing record
//! (which both devices hold). It is a small JSON file next to the trust store.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;

use nexus_core::config::ConsentMode;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
struct FileFormat {
    version: u32,
    /// Hex of the peer's Ed25519 key → mode, for peers that differ from the default.
    consent: BTreeMap<String, ConsentMode>,
}

/// Local per-peer policy.
#[derive(Debug)]
pub(crate) struct PolicyStore {
    path: Option<PathBuf>,
    default: ConsentMode,
    overrides: BTreeMap<String, ConsentMode>,
}

fn hex(key: &[u8; 32]) -> String {
    key.iter().map(|b| format!("{b:02x}")).collect()
}

impl PolicyStore {
    /// Loads the file (a missing or unreadable file means "no overrides"; the problem is logged
    /// and the safe default applies).
    pub(crate) fn load(path: PathBuf, default: ConsentMode) -> Self {
        let overrides = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<FileFormat>(&bytes) {
                Ok(f) => f.consent,
                Err(e) => {
                    tracing::warn!(error = %e, path = %path.display(), "policy file is corrupt; using defaults");
                    BTreeMap::new()
                }
            },
            Err(_) => BTreeMap::new(),
        };
        Self {
            path: Some(path),
            default,
            overrides,
        }
    }

    /// No persistence (tests, embedding).
    #[cfg(test)]
    pub(crate) fn in_memory(default: ConsentMode) -> Self {
        Self {
            path: None,
            default,
            overrides: BTreeMap::new(),
        }
    }

    pub(crate) fn consent(&self, peer: &[u8; 32]) -> ConsentMode {
        self.overrides
            .get(&hex(peer))
            .copied()
            .unwrap_or(self.default)
    }

    pub(crate) fn is_unattended(&self, peer: &[u8; 32]) -> bool {
        self.consent(peer) == ConsentMode::Unattended
    }

    pub(crate) fn set(&mut self, peer: &[u8; 32], mode: ConsentMode) -> std::io::Result<()> {
        let key = hex(peer);
        if mode == self.default {
            self.overrides.remove(&key);
        } else {
            self.overrides.insert(key, mode);
        }
        self.save()
    }

    /// Forgets a peer (unpaired).
    pub(crate) fn forget(&mut self, peer: &[u8; 32]) {
        if self.overrides.remove(&hex(peer)).is_some() {
            let _ = self.save();
        }
    }

    fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let json = serde_json::to_vec_pretty(&FileFormat {
            version: 1,
            consent: self.overrides.clone(),
        })
        .map_err(std::io::Error::other)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&json)?;
            f.sync_all()?;
        }
        std::fs::rename(tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_overrides_and_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policy.json");
        let (a, b) = ([1u8; 32], [2u8; 32]);
        let mut p = PolicyStore::load(path.clone(), ConsentMode::Attended);
        assert!(!p.is_unattended(&a));
        p.set(&a, ConsentMode::Unattended).unwrap();
        assert!(p.is_unattended(&a));
        assert!(!p.is_unattended(&b));

        let reloaded = PolicyStore::load(path.clone(), ConsentMode::Attended);
        assert!(reloaded.is_unattended(&a));
        assert!(!reloaded.is_unattended(&b));

        // Back to the default removes the override.
        let mut p = reloaded;
        p.set(&a, ConsentMode::Attended).unwrap();
        assert!(!PolicyStore::load(path.clone(), ConsentMode::Attended).is_unattended(&a));

        // Forgetting and corruption.
        p.set(&b, ConsentMode::Unattended).unwrap();
        p.forget(&b);
        assert!(!PolicyStore::load(path.clone(), ConsentMode::Attended).is_unattended(&b));
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(!PolicyStore::load(path, ConsentMode::Attended).is_unattended(&a));
    }

    #[test]
    fn a_default_of_unattended_applies_to_everyone_unless_overridden() {
        let mut p = PolicyStore::in_memory(ConsentMode::Unattended);
        let a = [9u8; 32];
        assert!(p.is_unattended(&a));
        p.set(&a, ConsentMode::Attended).unwrap();
        assert!(!p.is_unattended(&a));
    }
}
