//! The agent's local allow-list: the devices it is paired with and what each may do.
//!
//! Every entry is a full dual-signed record that is **re-verified when loaded**, so editing the
//! file by hand cannot add a trusted peer without that peer's valid signature (a local attacker
//! who can write this file can still *remove* peers, or replay an old file, which is why the
//! agent also keeps its allow-list in memory and treats the signaling server's revocation pushes
//! as authoritative while running).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use nexus_crypto::PublicIdentity;
use nexus_proto::v1::{SignedPairingRecord, SignedRevocation};
use prost::Message;
use serde::{Deserialize, Serialize};

use crate::error::TrustError;
use crate::record::{self, VerifiedPairing};
use crate::revocation;

const FORMAT_VERSION: u32 = 1;
/// Refuse to parse a store file larger than this (hundreds of peers fit in a fraction of it).
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// A device this one is paired with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedPeer {
    /// The peer's verified identity.
    pub identity: PublicIdentity,
    /// The name the peer gave itself when pairing.
    pub name: String,
    /// When the pairing was created.
    pub created_unix_ms: u64,
    /// What this device may do on the peer.
    pub grants_on_peer: BTreeSet<String>,
    /// What the peer may do on this device.
    pub grants_from_peer: BTreeSet<String>,
    /// The signed record (kept so it can be re-registered with a signaling server).
    pub signed: SignedPairingRecord,
}

#[derive(Serialize, Deserialize)]
struct FileFormat {
    version: u32,
    /// Base64 of each `SignedPairingRecord`.
    peers: Vec<String>,
    revoked: Vec<RevokedEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
struct RevokedEntry {
    peer: String,
    unix_ms: u64,
}

/// Persistent set of trusted peers for one local device.
#[derive(Debug)]
pub struct TrustStore {
    path: Option<PathBuf>,
    local: PublicIdentity,
    peers: BTreeMap<[u8; 32], PairedPeer>,
    /// Peers whose pairing was revoked, with the revocation time. A record created before this
    /// instant can never bring the peer back.
    revoked: BTreeMap<[u8; 32], u64>,
}

impl TrustStore {
    /// A store that lives only in memory (tests, ephemeral tools).
    #[must_use]
    pub fn in_memory(local: PublicIdentity) -> Self {
        Self {
            path: None,
            local,
            peers: BTreeMap::new(),
            revoked: BTreeMap::new(),
        }
    }

    /// Opens (or starts) the store at `path`, re-verifying every stored record.
    pub fn open(path: impl Into<PathBuf>, local: PublicIdentity) -> Result<Self, TrustError> {
        let path = path.into();
        let mut store = Self {
            path: Some(path.clone()),
            local,
            peers: BTreeMap::new(),
            revoked: BTreeMap::new(),
        };
        let meta = match fs::metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(store),
            Err(e) => return Err(e.into()),
        };
        if meta.len() > MAX_FILE_BYTES {
            return Err(TrustError::Corrupt("file too large".into()));
        }
        let text = fs::read_to_string(&path)?;
        let file: FileFormat =
            serde_json::from_str(&text).map_err(|e| TrustError::Corrupt(format!("json: {e}")))?;
        if file.version != FORMAT_VERSION {
            return Err(TrustError::Corrupt(format!(
                "unsupported version {}",
                file.version
            )));
        }
        for entry in file.revoked {
            let key = decode_key(&entry.peer)?;
            store.revoked.insert(key, entry.unix_ms);
        }
        for b64 in file.peers {
            let raw = data_encoding::BASE64
                .decode(b64.as_bytes())
                .map_err(|_| TrustError::Corrupt("bad base64".into()))?;
            let signed = SignedPairingRecord::decode(raw.as_slice())
                .map_err(|_| TrustError::Corrupt("bad record".into()))?;
            let verified = record::verify(&signed)
                .map_err(|e| TrustError::Corrupt(format!("stored record invalid: {e}")))?;
            store.insert(&verified)?;
        }
        Ok(store)
    }

    fn insert(&mut self, verified: &VerifiedPairing) -> Result<&PairedPeer, TrustError> {
        let me = self.local.ed25519_bytes();
        let view = verified.view_for(&me).ok_or(TrustError::NotAParty)?;
        let key = view.peer.ed25519_bytes();
        if let Some(revoked_at) = self.revoked.get(&key) {
            if verified.created_unix_ms <= *revoked_at {
                return Err(TrustError::Invalid(
                    "pairing was revoked after this record was created",
                ));
            }
            self.revoked.remove(&key);
        }
        if let Some(existing) = self.peers.get(&key)
            && existing.created_unix_ms > verified.created_unix_ms
        {
            return Err(TrustError::Invalid(
                "a newer pairing with this device exists",
            ));
        }
        self.peers.insert(
            key,
            PairedPeer {
                identity: view.peer,
                name: view.peer_name,
                created_unix_ms: verified.created_unix_ms,
                grants_on_peer: view.grants_on_peer.into_iter().collect(),
                grants_from_peer: view.grants_from_peer.into_iter().collect(),
                signed: verified.signed.clone(),
            },
        );
        self.peers
            .get(&key)
            .ok_or(TrustError::Invalid("insertion failed"))
    }

    /// Adds a pairing after fully verifying it. This device must be one of the two parties.
    pub fn add(&mut self, signed: &SignedPairingRecord) -> Result<&PairedPeer, TrustError> {
        let verified = record::verify(signed)?;
        // Take a copy of the key before the mutable borrow returned by `insert` ends.
        let key = verified
            .view_for(&self.local.ed25519_bytes())
            .ok_or(TrustError::NotAParty)?
            .peer
            .ed25519_bytes();
        self.insert(&verified)?;
        self.save()?;
        self.peers
            .get(&key)
            .ok_or(TrustError::Invalid("insertion failed"))
    }

    /// Removes `peer` and remembers the revocation. Returns whether the peer was present.
    pub fn revoke(&mut self, peer: &[u8; 32], now_ms: u64) -> Result<bool, TrustError> {
        let existed = self.peers.remove(peer).is_some();
        self.revoked.insert(*peer, now_ms);
        self.save()?;
        Ok(existed)
    }

    /// Applies a revocation pushed by the signaling server (or received any other way).
    /// It must be signed by a peer we are paired with and name *this* device as its target.
    pub fn apply_remote_revocation(
        &mut self,
        signed: &SignedRevocation,
        now_ms: u64,
    ) -> Result<[u8; 32], TrustError> {
        let r = revocation::verify(signed, now_ms)?;
        if r.peer != self.local.ed25519_bytes() {
            return Err(TrustError::NotAParty);
        }
        if !self.peers.contains_key(&r.revoker) {
            return Err(TrustError::NotAParty);
        }
        self.revoke(&r.revoker, r.unix_ms)?;
        Ok(r.revoker)
    }

    /// Whether `peer` (Ed25519 key) is currently paired.
    #[must_use]
    pub fn is_trusted(&self, peer: &[u8; 32]) -> bool {
        self.peers.contains_key(peer)
    }

    /// Whether `peer` may use `grant` on this device.
    #[must_use]
    pub fn allows(&self, peer: &[u8; 32], grant: &str) -> bool {
        self.peers
            .get(peer)
            .is_some_and(|p| p.grants_from_peer.contains(grant))
    }

    /// The peer record, if paired.
    #[must_use]
    pub fn get(&self, peer: &[u8; 32]) -> Option<&PairedPeer> {
        self.peers.get(peer)
    }

    /// All paired peers.
    pub fn peers(&self) -> impl Iterator<Item = &PairedPeer> {
        self.peers.values()
    }

    /// Ed25519 keys of all paired peers (the transport allow-list).
    #[must_use]
    pub fn allowlist(&self) -> Vec<[u8; 32]> {
        self.peers.keys().copied().collect()
    }

    /// Peers whose pairing was revoked locally, with the revocation time. The agent re-sends
    /// these to the signaling server after a reconnect, so a revocation made while offline still
    /// reaches the other party.
    pub fn revoked(&self) -> impl Iterator<Item = ([u8; 32], u64)> + '_ {
        self.revoked.iter().map(|(k, t)| (*k, *t))
    }

    /// The local device identity this store belongs to.
    #[must_use]
    pub fn local(&self) -> &PublicIdentity {
        &self.local
    }

    fn save(&self) -> Result<(), TrustError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let file = FileFormat {
            version: FORMAT_VERSION,
            peers: self
                .peers
                .values()
                .map(|p| data_encoding::BASE64.encode(&p.signed.encode_to_vec()))
                .collect(),
            revoked: self
                .revoked
                .iter()
                .map(|(k, t)| RevokedEntry {
                    peer: data_encoding::HEXLOWER.encode(k),
                    unix_ms: *t,
                })
                .collect(),
        };
        let json = serde_json::to_vec_pretty(&file)
            .map_err(|e| TrustError::Corrupt(format!("serialise: {e}")))?;
        write_private_atomic(path, &json)?;
        Ok(())
    }
}

fn decode_key(hex: &str) -> Result<[u8; 32], TrustError> {
    let raw = data_encoding::HEXLOWER
        .decode(hex.as_bytes())
        .map_err(|_| TrustError::Corrupt("bad key hex".into()))?;
    raw.try_into()
        .map_err(|_| TrustError::Corrupt("bad key length".into()))
}

/// Writes `data` to `path` via a private temp file + rename so a crash never leaves a torn file.
fn write_private_atomic(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        #[cfg(not(unix))]
        fs::create_dir_all(dir)?;
    }
    let mut nonce = [0u8; 6];
    getrandom::fill(&mut nonce).map_err(|e| std::io::Error::other(e.to_string()))?;
    let tmp = path.with_extension(format!("tmp-{}", data_encoding::HEXLOWER.encode(&nonce)));
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(&tmp)?;
    let result = file.write_all(data).and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = result.and_then(|()| fs::rename(&tmp, path)) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use nexus_crypto::{DeviceIdentity, IdentitySeed};

    use super::*;
    use crate::grants;

    const T0: u64 = 1_800_000_000_000;

    fn id(b: u8) -> DeviceIdentity {
        DeviceIdentity::from_seed(&IdentitySeed::from_bytes([b; 32]))
    }

    fn pair(
        x: &DeviceIdentity,
        y: &DeviceIdentity,
        at: u64,
        x_on_y: &[&str],
        y_on_x: &[&str],
    ) -> SignedPairingRecord {
        let own = |g: &[&str]| g.iter().map(ToString::to_string).collect::<Vec<_>>();
        let bytes = record::build_record(
            x.public(),
            "X",
            y.public(),
            "Y",
            &own(x_on_y),
            &own(y_on_x),
            at,
        )
        .unwrap();
        record::assemble(
            &bytes,
            x.public(),
            &record::sign_record(x, &bytes),
            &record::sign_record(y, &bytes),
        )
        .unwrap()
    }

    #[test]
    fn add_lookup_and_grants() {
        let (me, peer) = (id(1), id(2));
        let mut s = TrustStore::in_memory(me.public().clone());
        assert!(!s.is_trusted(&peer.public().ed25519_bytes()));
        let p = s
            .add(&pair(
                &me,
                &peer,
                T0,
                &[grants::SCREEN, grants::INPUT],
                &[grants::FILES],
            ))
            .unwrap();
        assert_eq!(p.identity, *peer.public());
        assert_eq!(p.name, "Y");
        assert!(s.is_trusted(&peer.public().ed25519_bytes()));
        assert!(
            s.allows(&peer.public().ed25519_bytes(), grants::FILES),
            "peer may use files on me"
        );
        assert!(
            !s.allows(&peer.public().ed25519_bytes(), grants::SCREEN),
            "I may view the peer, not vice versa"
        );
        assert_eq!(
            s.get(&peer.public().ed25519_bytes())
                .unwrap()
                .grants_on_peer
                .len(),
            2
        );
        assert_eq!(s.allowlist(), vec![peer.public().ed25519_bytes()]);
    }

    #[test]
    fn records_not_involving_this_device_are_refused() {
        let (me, a, b) = (id(1), id(2), id(3));
        let mut s = TrustStore::in_memory(me.public().clone());
        assert!(matches!(
            s.add(&pair(&a, &b, T0, &[], &[])),
            Err(TrustError::NotAParty)
        ));
        assert_eq!(s.peers().count(), 0);
    }

    #[test]
    fn invalid_records_are_refused() {
        let (me, peer) = (id(1), id(2));
        let mut s = TrustStore::in_memory(me.public().clone());
        let mut bad = pair(&me, &peer, T0, &[], &[]);
        bad.signature_b = bad.signature_a.clone();
        assert!(s.add(&bad).is_err());
        assert_eq!(s.peers().count(), 0);
    }

    #[test]
    fn revocation_removes_and_blocks_stale_records() {
        let (me, peer) = (id(1), id(2));
        let mut s = TrustStore::in_memory(me.public().clone());
        let old = pair(&me, &peer, T0, &[], &[]);
        s.add(&old).unwrap();
        assert!(s.revoke(&peer.public().ed25519_bytes(), T0 + 1000).unwrap());
        assert!(!s.is_trusted(&peer.public().ed25519_bytes()));
        // Replaying the old record must not resurrect the pairing...
        assert!(s.add(&old).is_err());
        // ...but a fresh pairing made later is legitimate.
        s.add(&pair(&me, &peer, T0 + 2000, &[], &[])).unwrap();
        assert!(s.is_trusted(&peer.public().ed25519_bytes()));
    }

    #[test]
    fn older_record_cannot_replace_newer() {
        let (me, peer) = (id(1), id(2));
        let mut s = TrustStore::in_memory(me.public().clone());
        s.add(&pair(&me, &peer, T0 + 10, &[grants::SCREEN], &[]))
            .unwrap();
        assert!(s.add(&pair(&me, &peer, T0, &[grants::POWER], &[])).is_err());
        assert!(
            s.get(&peer.public().ed25519_bytes())
                .unwrap()
                .grants_on_peer
                .contains(grants::SCREEN)
        );
    }

    #[test]
    fn remote_revocation_must_come_from_a_paired_peer_and_target_us() {
        let (me, peer, other, mallory) = (id(1), id(2), id(3), id(4));
        let mut s = TrustStore::in_memory(me.public().clone());
        s.add(&pair(&me, &peer, T0, &[], &[])).unwrap();
        s.add(&pair(&me, &other, T0, &[], &[])).unwrap();

        // Mallory (not paired) cannot revoke anyone.
        let forged = revocation::revoke(&mallory, me.public(), T0);
        assert!(matches!(
            s.apply_remote_revocation(&forged, T0),
            Err(TrustError::NotAParty)
        ));
        // A peer revoking some *other* device's pairing does not affect us.
        let not_us = revocation::revoke(&peer, other.public(), T0);
        assert!(s.apply_remote_revocation(&not_us, T0).is_err());
        assert!(s.is_trusted(&peer.public().ed25519_bytes()));
        // The real thing works and only removes that peer.
        let real = revocation::revoke(&peer, me.public(), T0 + 5);
        assert_eq!(
            s.apply_remote_revocation(&real, T0 + 5).unwrap(),
            peer.public().ed25519_bytes()
        );
        assert!(!s.is_trusted(&peer.public().ed25519_bytes()));
        assert!(s.is_trusted(&other.public().ed25519_bytes()));
    }

    #[test]
    fn persistence_roundtrip_with_reverification() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state").join("trust.json");
        let (me, a, b) = (id(1), id(2), id(3));
        {
            let mut s = TrustStore::open(&path, me.public().clone()).unwrap();
            s.add(&pair(&me, &a, T0, &[grants::SCREEN], &[])).unwrap();
            s.add(&pair(&me, &b, T0, &[], &[grants::FILES])).unwrap();
            s.revoke(&b.public().ed25519_bytes(), T0 + 1).unwrap();
        }
        let s = TrustStore::open(&path, me.public().clone()).unwrap();
        assert!(s.is_trusted(&a.public().ed25519_bytes()));
        assert!(
            !s.is_trusted(&b.public().ed25519_bytes()),
            "revocation persisted"
        );
        // A stale record for the revoked peer is still refused after a restart.
        let mut s = s;
        assert!(s.add(&pair(&me, &b, T0, &[], &[])).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn hand_edited_files_cannot_add_peers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let (me, a, mallory) = (id(1), id(2), id(9));
        {
            let mut s = TrustStore::open(&path, me.public().clone()).unwrap();
            s.add(&pair(&me, &a, T0, &[], &[])).unwrap();
        }
        // An attacker rewrites the stored record to name Mallory instead of the real peer.
        let text = fs::read_to_string(&path).unwrap();
        let forged = pair(&me, &mallory, T0, &[], &[]); // valid signatures, but not what's on file
        let mut file: FileFormat = serde_json::from_str(&text).unwrap();
        let mut tampered = forged.clone();
        tampered.signature_b = vec![7u8; 64].into(); // Mallory never signed this one
        file.peers = vec![data_encoding::BASE64.encode(&tampered.encode_to_vec())];
        fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        assert!(matches!(
            TrustStore::open(&path, me.public().clone()),
            Err(TrustError::Corrupt(_))
        ));
    }

    #[test]
    fn corrupt_files_are_errors_not_empty_stores() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let me = id(1);
        for junk in [
            "",
            "not json",
            "{\"version\":99,\"peers\":[],\"revoked\":[]}",
            "{\"version\":1,\"peers\":[\"!!!\"],\"revoked\":[]}",
        ] {
            fs::write(&path, junk).unwrap();
            assert!(
                TrustStore::open(&path, me.public().clone()).is_err(),
                "`{junk}` accepted"
            );
        }
    }
}
