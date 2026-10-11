# ADR-0006: Session security, consent, step-up authentication

**Status:** Accepted (design; transport part in Phase 2, consent UI Phase 5-8)

## Context
Sessions must be E2E encrypted with forward secrecy, authenticate both devices, resist replay
and downgrade, and keep the person at the target in control.

## Decision
**Transport security.** QUIC with TLS 1.3 and raw-public-key authentication (iroh). The dial
target's key *is* the expected peer; the accepting side rejects any key not in its paired
allow-list in the endpoint's accept hook, **before** application parsing. Forward secrecy comes
from ephemeral (EC)DHE in every handshake. Replay protection and AEAD are QUIC's. Key rotation:
QUIC key updates run continuously; the session layer additionally forces a fresh handshake
(reconnect with session resume) every 15 minutes or 1 GiB, whichever comes first.

**Downgrade protection.** TLS 1.3 only. `Hello` (inside the authenticated channel) carries each
side's `min_version`; negotiation refuses to settle below either floor
(`nexus_proto::version`).

**Consent (target-side policy, per paired controller):**
* *Attended:* a local prompt (Allow / Deny, timeout from config). No answer = deny.
* *Unattended:* pre-authorised but still requires **step-up**: a TOTP code (RFC 6238) or a
  WebAuthn assertion, verified **by the target agent itself**, not by any server, so a
  stolen controller key alone is insufficient. Server connections may require a hardware key.
* *Always visible:* tray icon + optional screen border while a session is active, a
  "Disconnect" action, and a panic hotkey.
* *Lockdown:* one switch refuses all remote access and drops live sessions.
* *Notifications:* new-connection events go to the owner's other devices.

**Abuse resistance.** Per-peer rate limiting and exponential lockout on failed step-up;
optional time-window and geofence rules.

## Consequences
The signaling server is out of the trust path for sessions; compromising it yields metadata
(who is paired, who is online, when), not access. Step-up verification code lives in the agent
and is fuzzed.
