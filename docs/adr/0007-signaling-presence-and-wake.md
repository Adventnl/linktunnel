# ADR-0007: Signaling, presence, wake

**Status:** Accepted (design; Phase 2 and Phase 8)

## Decision
* **Server:** `axum` + `tokio`, WebSocket carrying length-delimited protobuf. Postgres holds
  durable records (devices, pairings, audit), Redis holds ephemeral state (presence, rate
  limits). Both sit behind traits with an in-memory implementation used by tests and by
  single-node self-hosting.
* **Login:** the server sends a random nonce; the client signs
  `SignalAuth ‖ server_audience ‖ nonce ‖ client_nonce` with its device key. No bearer tokens
  to steal. Account-level actions needing a human (revoking from a lost device) use passkeys
  (WebAuthn) with TOTP fallback.
* **Presence:** agents publish a state machine value (`online`, `idle`, `locked`, `in_use`,
  `sleeping`, `hibernating`, `offline`) plus non-sensitive details; 5 s heartbeat with
  jitter and server TTL; a graceful *going to sleep / shutting down* message sent from OS
  power-event hooks makes offline detection instant. Subscribers receive presence **only for
  devices they hold a valid pairing with**.
* **Sealed envelopes:** small messages between paired devices (wake commands, session
  requests, notifications) are Noise `X`-pattern messages (sender-authenticated, receiver-
  encrypted) with a counter and timestamp for replay protection. The server routes
  ciphertext and cannot read or alter it.
* **Wake:** a controller sends a sealed `Wake{mac}` envelope to the paired `nexus-wake`
  helper on the target's LAN, which sends the magic packet. Smart-plug / IPMI / AMT drivers
  are pluggable later.

## Consequences
Signaling is horizontally scalable (stateless apart from the stores). Metadata exposure is
documented in the threat model.
