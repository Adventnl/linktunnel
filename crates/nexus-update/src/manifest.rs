//! The signed release manifest.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signature, VerifyingKey};
use nexus_crypto::{DeviceIdentity, SignContext, verify_with_key};
use semver::Version;
use serde::{Deserialize, Serialize};

/// Why an update step failed.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// No trusted publisher key signed the manifest.
    #[error("the manifest is not signed by a trusted publisher")]
    Untrusted,
    /// The manifest could not be parsed.
    #[error("malformed manifest: {0}")]
    Malformed(String),
    /// The manifest is past its validity period (a replayed old copy, or the publisher stopped
    /// refreshing it).
    #[error("the manifest has expired")]
    Expired,
    /// This install is too old to update directly to that release.
    #[error("this version is too old to update directly; install {min} or newer first")]
    TooOld {
        /// The minimum supported version.
        min: String,
    },
    /// A download did not match the hash or size the manifest promised.
    #[error("the downloaded file does not match the signed manifest")]
    HashMismatch,
    /// The download failed.
    #[error("download failed: {0}")]
    Download(String),
    /// A file operation failed.
    #[error("{0}")]
    Io(String),
}

impl From<std::io::Error> for UpdateError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

/// One downloadable file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    /// `windows`, `macos` or `linux`.
    pub os: String,
    /// `x86_64` or `aarch64`.
    pub arch: String,
    /// Where to fetch it. Not trusted: the hash below is what counts.
    pub url: String,
    /// BLAKE3 of the file, hex.
    pub blake3: String,
    /// Size in bytes.
    pub size: u64,
    /// What it is: `binary` (replace the program), `msi`, `pkg`, `deb`...
    pub kind: String,
}

/// The release description a publisher signs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// `stable`, `beta`, ...
    pub channel: String,
    /// Semantic version of the release.
    pub version: String,
    /// When it was published (ms since the epoch).
    pub released_ms: u64,
    /// After this instant the manifest is no longer accepted (defeats replay and freeze attacks).
    pub valid_until_ms: u64,
    /// Oldest version that may update directly to this one.
    #[serde(default)]
    pub min_supported: Option<String>,
    /// Percentage (0–100) of devices that may take this release now.
    pub rollout_percent: u8,
    /// Human-readable release notes.
    #[serde(default)]
    pub notes: String,
    /// The files.
    pub artifacts: Vec<Artifact>,
}

impl Manifest {
    /// The parsed version.
    pub fn parsed_version(&self) -> Result<Version, UpdateError> {
        Version::parse(&self.version)
            .map_err(|e| UpdateError::Malformed(format!("version {:?}: {e}", self.version)))
    }
}

/// A manifest plus the publisher's signature over its exact text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedManifest {
    /// The manifest as JSON text. Signing the text itself (not a re-serialisation) means there is
    /// nothing to canonicalise and nothing to disagree about.
    pub manifest: String,
    /// Base64 Ed25519 signature over `manifest` (context `UpdateManifest`).
    pub signature: String,
}

/// The publisher keys an install trusts.
#[derive(Debug, Clone, Default)]
pub struct PublisherKeys(Vec<VerifyingKey>);

impl PublisherKeys {
    /// Parses hex-encoded 32-byte Ed25519 public keys; unusable ones are rejected loudly.
    pub fn from_hex<S: AsRef<str>>(keys: &[S]) -> Result<Self, UpdateError> {
        let mut out = Vec::new();
        for k in keys {
            let k = k.as_ref().trim();
            let bytes: Vec<u8> = (0..k.len() / 2)
                .map(|i| u8::from_str_radix(k.get(i * 2..i * 2 + 2).unwrap_or("zz"), 16))
                .collect::<Result<_, _>>()
                .map_err(|_| UpdateError::Malformed(format!("publisher key {k:?} is not hex")))?;
            let arr: [u8; 32] = bytes
                .try_into()
                .map_err(|_| UpdateError::Malformed("publisher keys are 32 bytes".into()))?;
            out.push(
                VerifyingKey::from_bytes(&arr)
                    .map_err(|e| UpdateError::Malformed(format!("publisher key: {e}")))?,
            );
        }
        Ok(Self(out))
    }

    /// True if no key is configured (updates are then disabled, never "accept anything").
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Verifies the signature and parses the manifest. Nothing in the manifest is looked at
    /// before the signature checks out.
    pub fn verify(&self, signed: &SignedManifest) -> Result<Manifest, UpdateError> {
        let sig = B64
            .decode(signed.signature.trim())
            .ok()
            .and_then(|b| <[u8; 64]>::try_from(b.as_slice()).ok())
            .map(|b| Signature::from_bytes(&b))
            .ok_or(UpdateError::Untrusted)?;
        let ok = self.0.iter().any(|k| {
            verify_with_key(
                k,
                SignContext::UpdateManifest,
                signed.manifest.as_bytes(),
                &sig,
            )
            .is_ok()
        });
        if !ok {
            return Err(UpdateError::Untrusted);
        }
        serde_json::from_str(&signed.manifest).map_err(|e| UpdateError::Malformed(e.to_string()))
    }
}

/// Signs `manifest` with the publisher identity.
pub fn sign(
    publisher: &DeviceIdentity,
    manifest: &Manifest,
) -> Result<SignedManifest, UpdateError> {
    let text =
        serde_json::to_string(manifest).map_err(|e| UpdateError::Malformed(e.to_string()))?;
    let sig = publisher.sign(SignContext::UpdateManifest, text.as_bytes());
    Ok(SignedManifest {
        manifest: text,
        signature: B64.encode(sig.to_bytes()),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use nexus_crypto::IdentitySeed;

    use super::*;

    pub(crate) fn sample(version: &str) -> Manifest {
        Manifest {
            channel: "stable".into(),
            version: version.into(),
            released_ms: 0,
            valid_until_ms: 5_000_000,
            min_supported: None,
            rollout_percent: 100,
            notes: "Fixes.".into(),
            artifacts: vec![
                Artifact {
                    os: "linux".into(),
                    arch: "x86_64".into(),
                    url: "https://example.invalid/nexus".into(),
                    blake3: "00".repeat(32),
                    size: 10,
                    kind: "binary".into(),
                },
                Artifact {
                    os: "windows".into(),
                    arch: "x86_64".into(),
                    url: "https://example.invalid/nexus.msi".into(),
                    blake3: "11".repeat(32),
                    size: 20,
                    kind: "msi".into(),
                },
            ],
        }
    }

    fn publisher(byte: u8) -> DeviceIdentity {
        DeviceIdentity::from_seed(&IdentitySeed::from_bytes([byte; 32]))
    }

    fn keys_of(id: &DeviceIdentity) -> PublisherKeys {
        let hex: String = id
            .public()
            .ed25519_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        PublisherKeys::from_hex(&[hex]).unwrap()
    }

    #[test]
    fn a_signed_manifest_verifies_and_round_trips() {
        let p = publisher(1);
        let signed = sign(&p, &sample("1.2.3")).unwrap();
        assert_eq!(keys_of(&p).verify(&signed).unwrap(), sample("1.2.3"));
        // Through JSON, as it travels.
        let wire = serde_json::to_string(&signed).unwrap();
        let back: SignedManifest = serde_json::from_str(&wire).unwrap();
        assert!(keys_of(&p).verify(&back).is_ok());
    }

    #[test]
    fn tampering_a_wrong_key_or_no_key_is_rejected() {
        let p = publisher(1);
        let signed = sign(&p, &sample("1.2.3")).unwrap();
        // Changed text.
        let mut evil = signed.clone();
        evil.manifest = evil.manifest.replace("1.2.3", "9.9.9");
        assert!(matches!(
            keys_of(&p).verify(&evil),
            Err(UpdateError::Untrusted)
        ));
        // Someone else's signature.
        let other = sign(&publisher(2), &sample("1.2.3")).unwrap();
        assert!(matches!(
            keys_of(&p).verify(&other),
            Err(UpdateError::Untrusted)
        ));
        // No keys configured: never "accept anything".
        assert!(PublisherKeys::default().is_empty());
        assert!(matches!(
            PublisherKeys::default().verify(&signed),
            Err(UpdateError::Untrusted)
        ));
        // Garbage signature.
        let mut junk = signed.clone();
        junk.signature = "not base64!".into();
        assert!(matches!(
            keys_of(&p).verify(&junk),
            Err(UpdateError::Untrusted)
        ));
        // A signature made for another purpose does not verify as a manifest.
        let wrong_ctx = p.sign(SignContext::Envelope, signed.manifest.as_bytes());
        let mut ctx = signed;
        ctx.signature = B64.encode(wrong_ctx.to_bytes());
        assert!(matches!(
            keys_of(&p).verify(&ctx),
            Err(UpdateError::Untrusted)
        ));
    }

    #[test]
    fn bad_key_text_is_an_error() {
        assert!(PublisherKeys::from_hex(&["zz"]).is_err());
        assert!(PublisherKeys::from_hex(&["abcd"]).is_err());
    }
}
