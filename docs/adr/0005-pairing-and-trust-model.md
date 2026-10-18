# ADR-0005: Pairing, trust, revocation

**Status:** Accepted (design; implemented in Phase 2)

## Context
Zero-trust default: an unpaired device must not even learn that another device exists. Pairing
must defeat a man in the middle, including a malicious signaling server.

## Decision
* **Trust is a pairwise, mutually signed record.** `PairingRecord{a, b, permissions,
  created_at}` signed by both devices (context `PairingRecord`). The signaling server verifies
  both signatures before storing it and can therefore never invent a pairing.
* **Pairing protocol** (new device N, existing device E, both with a logged-in user present):
  1. N shows a QR code (rendezvous id + 32-byte PSK + N's public identity) and a 6-word
     phrase encoding the same PSK for manual entry.
  2. E scans (or the phrase is typed). Both connect to the signaling rendezvous (or the LAN,
     discovered over mDNS) and run **Noise `XXpsk3`** (25519 / ChaChaPoly / BLAKE2s) with the PSK.
  3. Both derive a **Short Authentication String** from the Noise handshake hash and show it;
     the humans confirm they match. A MITM would produce different hashes on each side.
  4. Both sign the `PairingRecord` and upload it.
* **Personal access:** successful initial pairing pre-authorises subsequent sessions by default.
  Per-device attended approval and per-session passkey/TOTP remain explicit options. Existing
  explicit approval and factor settings are preserved.
* **Revocation:** either party can sign a `Revocation`. The server marks the edge dead, stops
  presence/envelope delivery in both directions, and notifies the other party; a live agent
  drops matching sessions immediately. Allow-lists are also stored locally by each agent, so
  revocation holds even if the server is down once the agent learns of it.
* Pairing needs the second device's user to be present; no remote "approve" path exists.

## Consequences
N devices ⇒ up to N(N-1)/2 pairings. Acceptable for personal fleets; a transitive "introduce"
feature is possible later but deliberately excluded now (transitive trust widens blast radius).
