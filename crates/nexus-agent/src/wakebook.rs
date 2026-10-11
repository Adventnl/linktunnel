//! The hardware addresses of paired devices, remembered so they can be woken while asleep.
//!
//! A sleeping machine cannot tell us its MAC address, so it is learned while the device is awake
//! (from its presence announcement) and kept in `wake.json`.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
struct FileFormat {
    version: u32,
    /// Hex of the peer's Ed25519 key → MAC addresses.
    macs: BTreeMap<String, Vec<String>>,
}

/// Remembered MAC addresses per paired device.
#[derive(Debug)]
pub(crate) struct WakeBook {
    path: Option<PathBuf>,
    macs: BTreeMap<String, Vec<String>>,
}

fn hex(key: &[u8; 32]) -> String {
    key.iter().map(|b| format!("{b:02x}")).collect()
}

impl WakeBook {
    pub(crate) fn load(path: PathBuf) -> Self {
        let macs = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<FileFormat>(&b).ok())
            .map(|f| f.macs)
            .unwrap_or_default();
        Self {
            path: Some(path),
            macs,
        }
    }

    /// Remembers `macs` for `peer` (valid ones only); returns true if the book changed.
    pub(crate) fn learn(&mut self, peer: &[u8; 32], macs: &[String]) -> bool {
        let mut valid: Vec<String> = macs
            .iter()
            .filter(|m| nexus_power::parse_mac(m).is_ok())
            .map(|m| m.to_lowercase())
            .collect();
        valid.sort();
        valid.dedup();
        if valid.is_empty() {
            return false;
        }
        let key = hex(peer);
        if self.macs.get(&key) == Some(&valid) {
            return false;
        }
        self.macs.insert(key, valid);
        let _ = self.save();
        true
    }

    pub(crate) fn macs(&self, peer: &[u8; 32]) -> Vec<String> {
        self.macs.get(&hex(peer)).cloned().unwrap_or_default()
    }

    pub(crate) fn forget(&mut self, peer: &[u8; 32]) {
        if self.macs.remove(&hex(peer)).is_some() {
            let _ = self.save();
        }
    }

    fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let json = serde_json::to_vec_pretty(&FileFormat {
            version: 1,
            macs: self.macs.clone(),
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
    fn learns_validates_persists_and_forgets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wake.json");
        let peer = [7u8; 32];
        let mut b = WakeBook::load(path.clone());
        assert!(b.macs(&peer).is_empty());
        assert!(!b.learn(&peer, &["junk".into(), "00:00:00:00:00:00".into()]));
        assert!(b.learn(
            &peer,
            &["AA:BB:CC:DD:EE:01".into(), "aa:bb:cc:dd:ee:01".into()]
        ));
        assert!(!b.learn(&peer, &["aa:bb:cc:dd:ee:01".into()]), "unchanged");
        assert_eq!(
            WakeBook::load(path.clone()).macs(&peer),
            ["aa:bb:cc:dd:ee:01"]
        );
        b.forget(&peer);
        assert!(WakeBook::load(path).macs(&peer).is_empty());
    }
}
