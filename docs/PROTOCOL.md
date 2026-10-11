# Nexus protocol reference (v1)

Authoritative sources: `crates/nexus-proto/proto/nexus/v1/*.proto` (control plane),
`crates/nexus-proto/src/wire/*` (hot-path binary layouts, documented in each module). This page is
the map.

## 1. Three channels

| Channel | Between | Transport | Encoding | Trust |
|---|---|---|---|---|
| **Signaling** | device ↔ `nexus-signal` | WebSocket over TLS 1.3 | one protobuf message per binary frame | server is untrusted for content |
| **Session** | device ↔ device | QUIC (TLS 1.3 raw public keys, iroh), direct / hole-punched / relayed | streams + datagrams, below | end-to-end authenticated and encrypted |
| **IPC** | agent ↔ local client | Unix socket / named pipe | length-delimited protobuf | filesystem permissions + token + peer UID |

## 2. Versioning and evolution
* `Hello.version` / `min_version` are exchanged **inside** the authenticated session. Peers share a
  `major`; the agreed version is the lower of the two minors and must not fall below either side's
  `min_version` (downgrade protection). Tested in `nexus_proto::version`.
* Protobuf rules: never reuse or renumber a field; new behaviour = a new `oneof` variant plus a
  capability string (`nexus_proto::features`); unknown fields are ignored; unknown requests are
  answered with an error.
* Hot-path binary formats begin with a version byte and carry explicit lengths so a receiver can
  skip fields/events it does not know (input events) or reject cleanly (everything else).

## 3. Signaling (`signal.proto`)
1. Server → `ServerHello{version, nonce(32), audience}`.
2. Client → `ClientAuth{identity(128 B PublicIdentity), client_nonce(16), signature}` where
   `signature = Ed25519(SignalAuth-context ‖ len(audience) ‖ audience ‖ server_nonce ‖ client_nonce)`.
3. Server → `AuthOk{device_id, heartbeat_interval_ms, presence_ttl_ms, …}` or `SignalError`.
4. Requests (all answered with `Ack`, a typed reply, or `SignalError`): `PublishPresence`,
   `Heartbeat`, `GoingOffline`, `SubscribePresence`, `RegisterPairing`, `RevokePairing`,
   `ListPairings`, `SendEnvelope`, `RendezvousJoin/Send/Leave`.
5. Pushes: `PresenceUpdate`, `PairingAdded`, `PairingRevoked`, `IncomingEnvelope`, `RendezvousPeer`,
   `RendezvousData`, `Superseded`.

Authorization (enforced server-side, one test each in `nexus-signal/tests`): nothing before login;
pairing needs both signatures and the caller to be a party; revocation needs the revoker's
signature and the caller to *be* the revoker; presence and envelopes only between active pairings.

## 4. Pairing (`nexus-crypto::noise::pairing`, `nexus-trust::flow`)
```
 responder (shows code)                                   initiator (enters code)
   rendezvous_join(H(code))                                rendezvous_join(H(code))
                         ◄── Noise XXpsk0 (3 messages, PSK = KDF(code)) ──►
                         both derive SAS = 4 words from the handshake hash
   humans compare words; each confirms ──► CONFIRM (encrypted)
                         ◄── PROPOSAL{record, sig_initiator} ──
                         ──► COUNTERSIGN{sig_responder} ──►
   both hold the identical dual-signed PairingRecord; either registers it (RegisterPairing)
```
Noise protocol names: `Noise_XXpsk0_25519_ChaChaPoly_BLAKE2s` (pairing),
`Noise_X_25519_ChaChaPoly_BLAKE2s` (sealed envelopes). Prologues bind the rendezvous id /
recipient device id.

## 5. Session (`nexus-net`)
* **ALPN** `nexus/session/1`. TLS 1.3 raw-public-key auth with the device Ed25519 keys. The
  accepting side checks the paired allow-list **immediately after the handshake**; unpaired keys are
  closed with `0x4E01` before any application byte. Revocation closes live sessions with `0x4E02`;
  lockdown with `0x4E03`.
* **Streams**: first byte = `StreamKind` (control 1, input 2, clipboard 3, files 4, terminal 5,
  port-forward 6, video 16, diagnostics 250).
* **Control stream**: dialler sends `Hello`, acceptor answers; then `Ping/Pong`, `Goodbye`, `Error`,
  and later phases' RPCs. Frames are `varint(len) ‖ protobuf`, capped at 1 MiB.
* **Datagrams**: cursor updates and audio (formats in `nexus-proto::wire`).
* **Video**: one unidirectional stream per frame: 40-byte header + dirty rects + payload
  (Phase 3+).

## 6. IPC (`ipc.proto`)
`IpcHello{token}` → `IpcWelcome` → `IpcRequest`/`IpcResponse` correlated by id, plus pushed
`IpcEvent`s (`PairingSasReady`, `PairingFinished`, `DeviceChanged`, `DeviceRemoved`,
`SignalState`). The same API serves the CLI, the tray and the desktop app.
