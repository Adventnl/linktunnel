//! A tamper-evident record of what happened on this device.
//!
//! Every record carries the hash of the one before it, so editing or deleting a record in the
//! middle breaks the chain from that point on. The newest record's hash (the *head*) is signed
//! with the device identity and stored beside the log, so cutting records off the end is
//! detected too: the head claims a sequence number the log no longer reaches.
//!
//! What this does **not** do: stop someone with write access from deleting the whole directory,
//! or from rewriting the log *and* the head if they also hold the device's signing key. Its job
//! is to make quiet editing by anything weaker than the device itself detectable.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use nexus_crypto::{DeviceIdentity, PublicIdentity, SignContext, Signature};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

const LOG_NAME: &str = "audit.log";
const HEAD_NAME: &str = "audit.head";
/// Hash that precedes the first record.
const GENESIS: [u8; 32] = [0; 32];
/// Rotate the live log after this many bytes.
const DEFAULT_MAX_BYTES: u64 = 8 << 20;

/// Anything that can note an event. Components take an `Arc<dyn AuditSink>` so they do not need
/// to know about files.
pub trait AuditSink: Send + Sync {
    /// Notes that `kind` happened, optionally on behalf of `actor` (a device ID), with details.
    fn record(&self, kind: &str, actor: Option<&str>, detail: &[(&str, &str)]);
}

/// Discards everything.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullSink;

impl AuditSink for NullSink {
    fn record(&self, _kind: &str, _actor: Option<&str>, _detail: &[(&str, &str)]) {}
}

/// One recorded event: kind, actor, details.
pub type Event = (String, Option<String>, BTreeMap<String, String>);

/// Keeps events in memory (tests).
#[derive(Debug, Default, Clone)]
pub struct MemorySink {
    events: Arc<Mutex<Vec<Event>>>,
}

impl MemorySink {
    /// An empty sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything recorded so far.
    #[must_use]
    pub fn events(&self) -> Vec<Event> {
        self.events.lock().clone()
    }

    /// The kinds recorded so far, in order.
    #[must_use]
    pub fn kinds(&self) -> Vec<String> {
        self.events.lock().iter().map(|e| e.0.clone()).collect()
    }
}

impl AuditSink for MemorySink {
    fn record(&self, kind: &str, actor: Option<&str>, detail: &[(&str, &str)]) {
        self.events.lock().push((
            kind.to_owned(),
            actor.map(str::to_owned),
            detail
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        ));
    }
}

/// What gets hashed (field order is the serialisation order, which is stable).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Body {
    seq: u64,
    ts_ms: u64,
    kind: String,
    actor: Option<String>,
    detail: BTreeMap<String, String>,
}

/// One line of the log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Record {
    /// Position in the chain, starting at 1.
    pub seq: u64,
    /// Wall-clock time, milliseconds since the Unix epoch.
    pub ts_ms: u64,
    /// What happened, e.g. `session.started`.
    pub kind: String,
    /// The other device involved, if any.
    pub actor: Option<String>,
    /// Further facts, as text.
    pub detail: BTreeMap<String, String>,
    /// Hash of the previous record (hex).
    pub prev: String,
    /// Hash of this record (hex).
    pub hash: String,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

fn chain_hash(prev: &[u8; 32], body: &Body) -> Result<[u8; 32], AuditError> {
    let json = serde_json::to_vec(body).map_err(|e| AuditError::Io(e.to_string()))?;
    let mut h = blake3::Hasher::new_derive_key("nexus-remote 2026 audit chain");
    h.update(prev);
    h.update(&json);
    Ok(*h.finalize().as_bytes())
}

/// The signed pointer to the newest record.
#[derive(Debug, Serialize, Deserialize)]
struct Head {
    seq: u64,
    hash: String,
    /// Base64 Ed25519 signature over `seq || hash` (context `AuditEntry`).
    sig: String,
}

fn head_message(seq: u64, hash: &[u8; 32]) -> Vec<u8> {
    let mut m = seq.to_be_bytes().to_vec();
    m.extend_from_slice(hash);
    m
}

/// Why an audit operation failed.
#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    /// The log or head could not be read or written.
    #[error("audit log i/o: {0}")]
    Io(String),
}

impl From<std::io::Error> for AuditError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

struct Inner {
    file: File,
    bytes: u64,
    seq: u64,
    hash: [u8; 32],
}

/// An open audit log.
pub struct AuditLog {
    dir: PathBuf,
    signer: Option<Arc<DeviceIdentity>>,
    max_bytes: u64,
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for AuditLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditLog")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

fn read_records(path: &Path) -> Result<Vec<Result<Record, String>>, AuditError> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    Ok(BufReader::new(file)
        .lines()
        .map(|l| {
            let line = l.map_err(|e| e.to_string())?;
            serde_json::from_str::<Record>(&line).map_err(|e| format!("unreadable record: {e}"))
        })
        .collect())
}

/// The log files in chain order: rotated ones by first sequence number, then the live one.
fn log_files(dir: &Path) -> Result<Vec<PathBuf>, AuditError> {
    let mut rotated: Vec<(u64, PathBuf)> = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(num) = name
                .strip_prefix("audit.")
                .and_then(|r| r.strip_suffix(".log"))
                .and_then(|n| n.parse::<u64>().ok())
            {
                rotated.push((num, entry.path()));
            }
        }
    }
    rotated.sort();
    let mut files: Vec<PathBuf> = rotated.into_iter().map(|(_, p)| p).collect();
    files.push(dir.join(LOG_NAME));
    Ok(files)
}

impl AuditLog {
    /// Opens (creating if needed) the log in `dir`. With a `signer`, the head is signed after
    /// every record.
    pub fn open(dir: &Path, signer: Option<Arc<DeviceIdentity>>) -> Result<Self, AuditError> {
        Self::open_with(dir, signer, DEFAULT_MAX_BYTES)
    }

    /// Like [`open`](Self::open) with an explicit rotation size.
    pub fn open_with(
        dir: &Path,
        signer: Option<Arc<DeviceIdentity>>,
        max_bytes: u64,
    ) -> Result<Self, AuditError> {
        fs::create_dir_all(dir)?;
        // Continue the chain from the last intact record, wherever it lives.
        let (mut seq, mut hash) = (0u64, GENESIS);
        for path in log_files(dir)? {
            for rec in read_records(&path)?.into_iter().flatten() {
                if let Some(h) = unhex(&rec.hash) {
                    seq = rec.seq;
                    hash = h;
                }
            }
        }
        let path = dir.join(LOG_NAME);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let bytes = file.metadata()?.len();
        Ok(Self {
            dir: dir.to_path_buf(),
            signer,
            max_bytes,
            inner: Mutex::new(Inner {
                file,
                bytes,
                seq,
                hash,
            }),
        })
    }

    /// Appends a record and refreshes the signed head.
    pub fn append(
        &self,
        kind: &str,
        actor: Option<&str>,
        detail: &[(&str, &str)],
    ) -> Result<Record, AuditError> {
        let mut inner = self.inner.lock();
        let body = Body {
            seq: inner.seq + 1,
            ts_ms: unix_ms(),
            kind: kind.to_owned(),
            actor: actor.map(str::to_owned),
            detail: detail
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        };
        let hash = chain_hash(&inner.hash, &body)?;
        let record = Record {
            seq: body.seq,
            ts_ms: body.ts_ms,
            kind: body.kind,
            actor: body.actor,
            detail: body.detail,
            prev: hex(&inner.hash),
            hash: hex(&hash),
        };
        if inner.bytes >= self.max_bytes {
            self.rotate(&mut inner, record.seq)?;
        }
        let mut line = serde_json::to_vec(&record).map_err(|e| AuditError::Io(e.to_string()))?;
        line.push(b'\n');
        inner.file.write_all(&line)?;
        inner.file.sync_data()?;
        inner.bytes += line.len() as u64;
        inner.seq = record.seq;
        inner.hash = hash;
        self.write_head(record.seq, &hash)?;
        Ok(record)
    }

    fn rotate(&self, inner: &mut Inner, next_seq: u64) -> Result<(), AuditError> {
        let live = self.dir.join(LOG_NAME);
        // Named by the first sequence number it holds, so files sort into chain order.
        let first = read_records(&live)?
            .into_iter()
            .flatten()
            .map(|r| r.seq)
            .next()
            .unwrap_or(next_seq);
        fs::rename(&live, self.dir.join(format!("audit.{first:012}.log")))?;
        inner.file = OpenOptions::new().create(true).append(true).open(&live)?;
        inner.bytes = 0;
        Ok(())
    }

    fn write_head(&self, seq: u64, hash: &[u8; 32]) -> Result<(), AuditError> {
        let Some(signer) = &self.signer else {
            return Ok(());
        };
        let sig = signer.sign(SignContext::AuditEntry, &head_message(seq, hash));
        let head = Head {
            seq,
            hash: hex(hash),
            sig: B64.encode(sig.to_bytes()),
        };
        let json = serde_json::to_vec(&head).map_err(|e| AuditError::Io(e.to_string()))?;
        let tmp = self.dir.join("audit.head.tmp");
        {
            let mut f = File::create(&tmp)?;
            f.write_all(&json)?;
            f.sync_data()?;
        }
        fs::rename(tmp, self.dir.join(HEAD_NAME))?;
        Ok(())
    }

    /// The most recent `n` records, oldest first.
    pub fn recent(&self, n: usize) -> Result<Vec<Record>, AuditError> {
        let mut all = Vec::new();
        for path in log_files(&self.dir)? {
            all.extend(read_records(&path)?.into_iter().flatten());
        }
        let skip = all.len().saturating_sub(n);
        Ok(all.split_off(skip))
    }
}

impl AuditSink for AuditLog {
    fn record(&self, kind: &str, actor: Option<&str>, detail: &[(&str, &str)]) {
        if let Err(e) = self.append(kind, actor, detail) {
            tracing::error!(error = %e, kind, "could not write the audit log");
        }
    }
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0))
}

/// What [`verify`] found.
#[derive(Debug, PartialEq, Eq)]
pub struct Verification {
    /// Records checked.
    pub records: u64,
    /// The first problem, if any: sequence number (0 if not tied to a record) and what is wrong.
    pub problem: Option<(u64, String)>,
    /// Whether a signed head was present and valid.
    pub head_verified: bool,
}

impl Verification {
    /// No problems found.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.problem.is_none()
    }
}

/// Walks the whole chain in `dir`. With `verifier`, the signed head must also be present, valid
/// and consistent with the log (so a truncated tail is caught).
pub fn verify(dir: &Path, verifier: Option<&PublicIdentity>) -> Result<Verification, AuditError> {
    let (mut prev, mut expected_seq, mut records) = (GENESIS, 1u64, 0u64);
    let bad = |seq: u64, why: &str, records: u64| Verification {
        records,
        problem: Some((seq, why.to_owned())),
        head_verified: false,
    };
    for path in log_files(dir)? {
        for rec in read_records(&path)? {
            let rec = match rec {
                Ok(r) => r,
                Err(why) => return Ok(bad(expected_seq, &why, records)),
            };
            if rec.seq != expected_seq {
                return Ok(bad(
                    rec.seq,
                    &format!("expected record {expected_seq}: a record is missing or out of order"),
                    records,
                ));
            }
            if unhex(&rec.prev) != Some(prev) {
                return Ok(bad(rec.seq, "does not follow the previous record", records));
            }
            let body = Body {
                seq: rec.seq,
                ts_ms: rec.ts_ms,
                kind: rec.kind.clone(),
                actor: rec.actor.clone(),
                detail: rec.detail.clone(),
            };
            let want = chain_hash(&prev, &body)?;
            if unhex(&rec.hash) != Some(want) {
                return Ok(bad(rec.seq, "the record was modified", records));
            }
            prev = want;
            expected_seq += 1;
            records += 1;
        }
    }
    let mut head_verified = false;
    if let Some(key) = verifier {
        let head: Option<Head> = fs::read(dir.join(HEAD_NAME))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());
        let Some(head) = head else {
            if records == 0 {
                return Ok(Verification {
                    records,
                    problem: None,
                    head_verified: false,
                });
            }
            return Ok(bad(0, "the signed head is missing", records));
        };
        let sig = B64
            .decode(&head.sig)
            .ok()
            .and_then(|b| <[u8; 64]>::try_from(b.as_slice()).ok())
            .map(|b| Signature::from_bytes(&b));
        let hash = unhex(&head.hash);
        let (Some(sig), Some(hash)) = (sig, hash) else {
            return Ok(bad(0, "the signed head is malformed", records));
        };
        if key
            .verify(
                SignContext::AuditEntry,
                &head_message(head.seq, &hash),
                &sig,
            )
            .is_err()
        {
            return Ok(bad(
                head.seq,
                "the signed head has a bad signature",
                records,
            ));
        }
        if head.seq != records || hash != prev {
            return Ok(bad(
                head.seq,
                "the log does not reach the signed head: records were cut off the end",
                records,
            ));
        }
        head_verified = true;
    }
    Ok(Verification {
        records,
        problem: None,
        head_verified,
    })
}

#[cfg(test)]
mod tests {
    use nexus_crypto::IdentitySeed;

    use super::*;

    fn identity() -> Arc<DeviceIdentity> {
        Arc::new(DeviceIdentity::from_seed(&IdentitySeed::from_bytes(
            [9; 32],
        )))
    }

    fn filled(dir: &Path, n: usize) -> Arc<DeviceIdentity> {
        let id = identity();
        let log = AuditLog::open(dir, Some(Arc::clone(&id))).unwrap();
        for i in 0..n {
            log.append(
                "session.started",
                Some("peer-1"),
                &[("n", &i.to_string()), ("path", "direct")],
            )
            .unwrap();
        }
        id
    }

    #[test]
    fn a_clean_log_verifies_and_survives_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let id = filled(dir.path(), 5);
        let v = verify(dir.path(), Some(id.public())).unwrap();
        assert!(v.is_clean(), "{v:?}");
        assert_eq!((v.records, v.head_verified), (5, true));
        // Reopen and continue the chain.
        let log = AuditLog::open(dir.path(), Some(Arc::clone(&id))).unwrap();
        log.append("lockdown.on", None, &[]).unwrap();
        let v = verify(dir.path(), Some(id.public())).unwrap();
        assert!(v.is_clean() && v.records == 6, "{v:?}");
        assert_eq!(log.recent(2).unwrap().len(), 2);
        assert_eq!(log.recent(100).unwrap().len(), 6);
    }

    #[test]
    fn editing_deleting_or_reordering_a_record_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let id = filled(dir.path(), 6);
        let path = dir.path().join(LOG_NAME);
        let original = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = original.lines().collect();

        // Edit a field.
        let edited = original.replace("\"n\":\"2\"", "\"n\":\"9\"");
        fs::write(&path, &edited).unwrap();
        let v = verify(dir.path(), Some(id.public())).unwrap();
        assert_eq!(v.problem.as_ref().map(|p| p.0), Some(3), "{v:?}");

        // Delete a record from the middle.
        let mut cut = lines.clone();
        cut.remove(2);
        fs::write(&path, cut.join("\n") + "\n").unwrap();
        assert!(
            verify(dir.path(), Some(id.public()))
                .unwrap()
                .problem
                .is_some()
        );

        // Swap two records.
        let mut swapped = lines.clone();
        swapped.swap(1, 2);
        fs::write(&path, swapped.join("\n") + "\n").unwrap();
        assert!(
            verify(dir.path(), Some(id.public()))
                .unwrap()
                .problem
                .is_some()
        );

        // Garbage.
        fs::write(&path, "not json\n").unwrap();
        assert!(verify(dir.path(), None).unwrap().problem.is_some());
    }

    #[test]
    fn cutting_records_off_the_end_is_caught_by_the_signed_head() {
        let dir = tempfile::tempdir().unwrap();
        let id = filled(dir.path(), 6);
        let path = dir.path().join(LOG_NAME);
        let text = fs::read_to_string(&path).unwrap();
        let kept: Vec<&str> = text.lines().take(4).collect();
        fs::write(&path, kept.join("\n") + "\n").unwrap();
        // The chain itself is still internally valid …
        assert!(verify(dir.path(), None).unwrap().is_clean());
        // … but it no longer reaches the head the device signed.
        let v = verify(dir.path(), Some(id.public())).unwrap();
        assert!(
            v.problem.as_ref().is_some_and(|p| p.1.contains("cut off")),
            "{v:?}"
        );
    }

    #[test]
    fn a_forged_or_foreign_head_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let id = filled(dir.path(), 3);
        let other = DeviceIdentity::from_seed(&IdentitySeed::from_bytes([1; 32]));
        assert!(
            verify(dir.path(), Some(other.public()))
                .unwrap()
                .problem
                .is_some()
        );
        fs::remove_file(dir.path().join(HEAD_NAME)).unwrap();
        let v = verify(dir.path(), Some(id.public())).unwrap();
        assert!(
            v.problem
                .as_ref()
                .is_some_and(|p| p.1.contains("head is missing"))
        );
        // Without a verifier the chain alone is accepted.
        assert!(verify(dir.path(), None).unwrap().is_clean());
    }

    #[test]
    fn rotation_keeps_the_chain_unbroken() {
        let dir = tempfile::tempdir().unwrap();
        let id = identity();
        let log = AuditLog::open_with(dir.path(), Some(Arc::clone(&id)), 300).unwrap();
        for i in 0..20 {
            log.append("x", None, &[("i", &i.to_string())]).unwrap();
        }
        let rotated = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("audit.0"))
            .count();
        assert!(
            rotated >= 2,
            "expected rotation, found {rotated} rotated files"
        );
        let v = verify(dir.path(), Some(id.public())).unwrap();
        assert!(v.is_clean() && v.records == 20, "{v:?}");
        // Reopening after rotation continues, not restarts.
        let log = AuditLog::open_with(dir.path(), Some(id.clone()), 300).unwrap();
        log.append("y", None, &[]).unwrap();
        assert!(verify(dir.path(), Some(id.public())).unwrap().records == 21);
    }

    #[test]
    fn sinks_record_without_panicking() {
        NullSink.record("anything", None, &[]);
        let m = MemorySink::new();
        m.record("a", Some("p"), &[("k", "v")]);
        m.record("b", None, &[]);
        assert_eq!(m.kinds(), ["a", "b"]);
        assert_eq!(m.events()[0].2.get("k").map(String::as_str), Some("v"));
    }
}
