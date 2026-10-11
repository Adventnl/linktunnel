# ADR-0013: Audit log

**Status:** Accepted (design; Phase 9)

## Decision
Append-only, hash-chained, signed: each entry contains `{seq, time, actor device, event,
details, prev_hash}`; `hash = BLAKE3(entry)`; every Nth entry (and on shutdown) carries an
Ed25519 checkpoint signature over the chain head (context `AuditEntry`). Events: connection
requested/allowed/denied, path type, duration, features used, file transfers (names + hashes),
terminal commands (opt-in), policy changes, lockdown, revocations. Verification (`nexus audit
verify`) detects truncation, reordering and edits. Optional encrypted session recording is
local-only and off by default.

Limits: an attacker with the device key can re-sign a truncated chain; periodic checkpoints are
also mirrored to the owner's other devices so truncation is detectable from outside.
