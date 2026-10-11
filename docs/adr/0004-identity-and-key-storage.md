# ADR-0004: Device identity and key storage

**Status:** Accepted (implemented in `nexus-crypto`)

## Context
Mutual device authentication with long-term keys, no passwords, secure storage on three
OSes, headless servers included.

## Decision
* One **32-byte random root seed** per device. From it, BLAKE3 `derive_key` (distinct context
  strings) derives an **Ed25519** signing key and an **X25519** static key. The X25519 key is
  bound to the Ed25519 key by a signature, so a peer that trusts the Ed25519 key can trust the
  X25519 key. Signing and DH keys are separate on purpose.
* `DeviceId` = 128 bits of a BLAKE3-derived hash of the Ed25519 public key (display only;
  trust decisions compare full keys). `Fingerprint` = 100-bit grouped form for reading aloud.
* **Domain-separated signatures:** every signature covers `prefix ‖ purpose label ‖ message`;
  a login proof can never be replayed as a pairing record. Verification is `verify_strict` and
  rejects small-order keys.
* **Storage backends:** OS keychain (`keyring`: macOS Keychain, Windows Credential Manager,
  Linux Secret Service) for interactive users; owner-only key file (`0600`, owner checked,
  atomic create, checksum) for headless servers and system services. Creation uses an atomic
  create-if-absent so two processes starting together agree on one identity.
* The derivation is pinned by a **known-answer test** whose expected values were produced by an
  independent implementation (Python `blake3` + OpenSSL), so a refactor cannot silently orphan
  deployed devices.

## Not done, and why
* **Secure Enclave / TPM binding:** the Secure Enclave only holds P-256 keys, and TPM 2.0
  Ed25519 support is not universal. The portable design keeps a software seed; wrapping the seed
  with a hardware-bound key where available is a hardening item.
* **Windows system scope** needs a DPAPI-wrapped file (Phase 6); the plain file store relies on
  the ACL of `%ProgramData%\Nexus`, which the installer must lock down.

## Consequences
Losing the seed means re-pairing every peer, by design. Deleting the identity is an explicit,
confirmed action in the CLI.
