# ADR-0014: Updates and release signing

**Status:** Accepted (design; Phases 9-10)

## Decision
Update manifests are signed with **minisign** (Ed25519); the agent embeds the public key(s) and
verifies manifest, then artifact hash, before staging. Rollback protection via a monotonically
increasing manifest `sequence` stored locally and a minimum-version field; staged rollout via a
deterministic per-device bucket (`hash(device_id) mod 100 < rollout_percent`). Installers are
additionally OS-signed (Authenticode, Developer ID + notarisation). Key custody and
hosting of the update service are the owner's decision; the design assumes an offline root key
that signs manifests.

Reproducible builds are a stretch goal; SBOMs (CycloneDX) are generated per release.
