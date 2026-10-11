//! Updates that cannot be forged, replayed, or half-applied.
//!
//! The chain of trust is deliberately short:
//!
//! 1. The publisher signs a *manifest* (version, artifact hashes, expiry) with an Ed25519 key that
//!    never leaves the release machine ([`manifest::sign`], used by the `nexus-release` tool).
//! 2. Every install carries the publisher's public key(s) ([`PublisherKeys`]); a manifest with no
//!    valid signature from one of them is rejected before anything is parsed or downloaded.
//! 3. The manifest names each artifact by BLAKE3 hash and size; a download that does not match is
//!    deleted ([`fetch::download`]). The download URL is therefore not trusted at all.
//! 4. Old manifests cannot be replayed to downgrade or freeze an install: manifests expire, and a
//!    version at or below the running one is ignored ([`decide`]).
//! 5. Applying keeps the previous binary next to the new one so a bad release can be rolled back
//!    ([`apply`]).
//!
//! A *rollout percentage* in the manifest lets a release reach a stable slice of devices first;
//! each device falls into the same slice every time (hash of its ID and the version).

#![forbid(unsafe_code)]

pub mod apply;
pub mod fetch;
pub mod manifest;

pub use manifest::{Artifact, Manifest, PublisherKeys, SignedManifest, UpdateError};

use semver::Version;

/// What to do about a verified manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Nothing newer.
    UpToDate,
    /// A newer version exists but this device is not in the current rollout slice yet.
    NotYet {
        /// The version that will arrive.
        version: Version,
        /// How much of the fleet may have it now.
        rollout_percent: u8,
    },
    /// Install this.
    Update {
        /// The new version.
        version: Version,
        /// The file for this OS and CPU.
        artifact: Artifact,
        /// Release notes.
        notes: String,
    },
    /// The manifest is valid but offers nothing for this platform.
    NoArtifact(Version),
}

/// Facts about the running device that decide eligibility.
#[derive(Debug, Clone)]
pub struct Device<'a> {
    /// Running version.
    pub current: &'a Version,
    /// `windows`, `macos` or `linux`.
    pub os: &'a str,
    /// `x86_64` or `aarch64`.
    pub arch: &'a str,
    /// Stable per-device bytes (the device ID) for rollout bucketing.
    pub device_id: &'a [u8],
    /// Now, milliseconds since the Unix epoch.
    pub now_ms: u64,
    /// Channel this device follows (`stable`, `beta`).
    pub channel: &'a str,
}

/// Which of 100 buckets a device falls into for a given release.
#[must_use]
pub fn rollout_bucket(device_id: &[u8], version: &Version) -> u8 {
    let mut h = blake3::Hasher::new();
    h.update(device_id);
    h.update(version.to_string().as_bytes());
    h.finalize().as_bytes()[0] % 100
}

/// Decides what a verified `manifest` means for `device`.
pub fn decide(manifest: &Manifest, device: &Device<'_>) -> Result<Decision, UpdateError> {
    if manifest.channel != device.channel {
        return Ok(Decision::UpToDate);
    }
    if device.now_ms > manifest.valid_until_ms {
        return Err(UpdateError::Expired);
    }
    let version = manifest.parsed_version()?;
    // Anything not strictly newer is ignored: a replayed old manifest changes nothing.
    if &version <= device.current {
        return Ok(Decision::UpToDate);
    }
    if let Some(min) = &manifest.min_supported
        && let Ok(min) = Version::parse(min)
        && device.current < &min
    {
        // Too old to jump straight there; the publisher must offer an intermediate release.
        return Err(UpdateError::TooOld {
            min: min.to_string(),
        });
    }
    if rollout_bucket(device.device_id, &version) >= manifest.rollout_percent.min(100) {
        return Ok(Decision::NotYet {
            version,
            rollout_percent: manifest.rollout_percent,
        });
    }
    let Some(artifact) = manifest
        .artifacts
        .iter()
        .find(|a| a.os == device.os && a.arch == device.arch)
        .cloned()
    else {
        return Ok(Decision::NoArtifact(version));
    };
    Ok(Decision::Update {
        version,
        artifact,
        notes: manifest.notes.clone(),
    })
}

/// `windows`, `macos` or `linux` for the running build.
#[must_use]
pub const fn current_os() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// `x86_64` or `aarch64` for the running build.
#[must_use]
pub const fn current_arch() -> &'static str {
    std::env::consts::ARCH
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::tests::sample;

    fn dev<'a>(current: &'a Version, id: &'a [u8]) -> Device<'a> {
        Device {
            current,
            os: "linux",
            arch: "x86_64",
            device_id: id,
            now_ms: 1_000,
            channel: "stable",
        }
    }

    #[test]
    fn a_newer_version_for_this_platform_is_offered() {
        let m = sample("1.2.0");
        let cur = Version::parse("1.1.0").unwrap();
        match decide(&m, &dev(&cur, b"device-a")).unwrap() {
            Decision::Update {
                version, artifact, ..
            } => {
                assert_eq!(version.to_string(), "1.2.0");
                assert_eq!(artifact.os, "linux");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn equal_older_and_other_channel_manifests_change_nothing() {
        let cur = Version::parse("1.2.0").unwrap();
        assert_eq!(
            decide(&sample("1.2.0"), &dev(&cur, b"x")).unwrap(),
            Decision::UpToDate
        );
        assert_eq!(
            decide(&sample("1.1.9"), &dev(&cur, b"x")).unwrap(),
            Decision::UpToDate
        );
        let mut beta = sample("2.0.0");
        beta.channel = "beta".into();
        assert_eq!(decide(&beta, &dev(&cur, b"x")).unwrap(), Decision::UpToDate);
    }

    #[test]
    fn expired_manifests_are_rejected_not_ignored() {
        let cur = Version::parse("1.0.0").unwrap();
        let mut d = dev(&cur, b"x");
        d.now_ms = 10_000_000;
        assert!(matches!(
            decide(&sample("1.2.0"), &d),
            Err(UpdateError::Expired)
        ));
    }

    #[test]
    fn rollout_is_stable_per_device_and_roughly_proportional() {
        let v = Version::parse("3.0.0").unwrap();
        // Same device, same answer.
        assert_eq!(rollout_bucket(b"dev", &v), rollout_bucket(b"dev", &v));
        let mut m = sample("3.0.0");
        m.rollout_percent = 25;
        let cur = Version::parse("2.0.0").unwrap();
        let eligible = (0..2000u32)
            .filter(|i| {
                matches!(
                    decide(&m, &dev(&cur, &i.to_be_bytes())).unwrap(),
                    Decision::Update { .. }
                )
            })
            .count();
        assert!((350..650).contains(&eligible), "{eligible} of 2000 at 25%");
        m.rollout_percent = 0;
        assert!(matches!(
            decide(&m, &dev(&cur, b"anyone")).unwrap(),
            Decision::NotYet { .. }
        ));
        m.rollout_percent = 100;
        assert!(matches!(
            decide(&m, &dev(&cur, b"anyone")).unwrap(),
            Decision::Update { .. }
        ));
    }

    #[test]
    fn platform_mismatch_and_too_old_installs_are_reported() {
        let cur = Version::parse("1.0.0").unwrap();
        let mut d = dev(&cur, b"x");
        d.os = "windows";
        d.arch = "aarch64";
        assert!(matches!(
            decide(&sample("1.2.0"), &d).unwrap(),
            Decision::NoArtifact(_)
        ));
        let mut m = sample("5.0.0");
        m.min_supported = Some("4.0.0".into());
        assert!(matches!(
            decide(&m, &dev(&cur, b"x")),
            Err(UpdateError::TooOld { .. })
        ));
    }
}
