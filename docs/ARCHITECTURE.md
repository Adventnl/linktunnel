# Nexus Remote: Architecture

*One-page summary first; details and rationale live in the [ADRs](adr/README.md).*

## 1. What it is

A personal remote-control suite for devices **you own or are authorised to administer**. Every
machine runs the same agent; any machine can be a controller, any can be a target. A dashboard
shows live presence (online / locked / in use / sleeping / offline) of your paired devices and
lets you take control of one, run a terminal, move files, or wake it.

## 2. The five ideas the design rests on

1. **One identity per device.** A 32-byte root seed in the OS secret store derives an Ed25519
   key (the device's identity *and* its transport endpoint id) and an X25519 key (Noise). No
   passwords, no certificate authority. Trust = "I paired with this exact key".
   ([ADR-0004](adr/0004-identity-and-key-storage.md), [ADR-0005](adr/0005-pairing-and-trust-model.md))
2. **The servers are untrusted.** The signaling server knows *who is paired with whom* and
   *who is online*; it relays small sealed envelopes. It never sees pixels, keystrokes or
   file contents, and cannot forge a pairing (both devices sign it). Relays forward
   TLS-1.3-encrypted QUIC and nothing else. ([ADR-0007](adr/0007-signaling-presence-and-wake.md))
3. **QUIC end to end, NAT traversal solved by a proven stack.** Live sessions are QUIC
   connections dialled by public key (iroh): direct on the LAN, hole-punched across NATs,
   HTTPS-relayed as a last resort, migrating to a better path mid-session.
   ([ADR-0003](adr/0003-transport-quic-via-iroh.md))
4. **Latency is a feature with a budget.** Capture → encode → network → decode → present is
   instrumented at every hop; the cursor is drawn locally; frames travel on per-frame QUIC
   streams that are *reset* when stale instead of delivered late.
   ([ADR-0008](adr/0008-wire-protocol-and-stream-mapping.md), [ADR-0009](adr/0009-media-pipeline-and-codecs.md))
5. **Privilege is split and the target is always in charge.** A small privileged helper does
   only what needs privilege (inject input, capture the secure desktop); network-facing code
   runs unprivileged. The person at the target sees a persistent indicator, can disconnect, and
   can flip a lockdown switch. ([ADR-0006](adr/0006-session-security-and-consent.md),
   [ADR-0011](adr/0011-input-injection-and-privilege-separation.md))

## 3. Components

```
 ┌───────────────┐  presence, pairing registry,   ┌────────────────┐
 │  nexus-app /  │◄──── sealed envelopes ────────►│  nexus-signal  │  axum + Postgres + Redis
 │  nexus-cli    │          (WSS, TLS 1.3)        │ (untrusted for │
 └──────┬────────┘                                │   content)     │
        │ QUIC (TLS 1.3, keys pinned to paired    └────────────────┘
        │ device ids) direct / hole-punched / relay           ▲
        ▼                                          presence   │
 ┌───────────────┐  IPC (authenticated local)  ┌──────────────┴──┐   ┌──────────────┐
 │  nexus-agent  │◄───────────────────────────►│ nexus-session-  │   │ nexus-relay  │
 │ (every device)│                             │ helper (priv.)  │   │ (iroh-relay, │
 └───────────────┘                             └─────────────────┘   │  encrypted)  │
        ▲  sealed wake command via signaling                         └──────────────┘
 ┌──────┴────────┐
 │  nexus-wake   │  always-on LAN helper: sends Wake-on-LAN magic packets
 └───────────────┘
```

| Crate | Responsibility |
|---|---|
| `nexus-core` | config, paths, tracing, latency histograms, i18n, monotonic clock |
| `nexus-proto` | protobuf control messages, stream framing, hot-path binary codecs, version negotiation |
| `nexus-crypto` | device identity, domain-separated signatures, secret stores (Noise pairing + envelopes in Phase 2) |
| `nexus-net` | iroh endpoint, allowlist enforcement, stream/datagram plumbing, path telemetry |
| `nexus-capture` / `-codec` / `-render` / `-input` / `-audio` / `-clipboard` / `-files` / `-terminal` / `-power` | per-feature, per-OS backends behind traits |
| `nexus-agent` | the daemon: presence, sessions, consent policy, audit |
| `nexus-signal` | signaling / presence / pairing registry server |
| *(`nexus-session-helper`, `nexus-wake`, `nexus-relay`)* | **not separate binaries**: the Windows "helper" is the agent itself in the console session (ADR-0018); the wake helper is `nexus-agent --wake-helper`; relays are stock `iroh-relay` (ADR-0003) |
| `nexus-audio` | Opus codec, jitter buffer, packetizer, player, cpal devices (feature `device`) |
| `nexus-vdisplay` | supervised Xvfb virtual display for headless Linux |
| `nexus-discovery` | privacy-preserving mDNS LAN discovery |
| `nexus-desktop` | Windows input-desktop tracking (Default ⇄ Winlogon) |
| `nexus-winservice` | Windows service: SYSTEM agent in the console session, supervisor, consent prompts |
| `nexus-media`, `nexus-render`, `nexus-files`, `nexus-terminal`, `nexus-portfwd`, `nexus-clipboard`, `nexus-power`, `nexus-presence`, `nexus-audit`, `nexus-update`, `nexus-trust`, `nexus-ipc`, `nexus-signal-client` | the session machinery, viewer, file panel, PTY terminals, port forwarding, clipboard, power/WoL, presence sensors, signed audit log, signed updates, pairing/trust, local IPC, signaling client |
| `nexus-cli` | `nexus` command: identity, devices, pairing, remote view/screenshot, files, shell, forward, proxy, power, wake, dashboard (TUI), audit, update, lockdown |
| `apps/nexus-app` | Tauri 2 desktop app (dashboard, tray, terminal, pairing, consent): a thin client of the agent IPC |
| `fuzz/` | 13 cargo-fuzz targets |

## 4. Connection flow (as built)

1. Agent boots → loads identity → authenticates to `nexus-signal` by signing a server nonce
   (domain-separated, audience-bound) → publishes presence; heartbeat + server TTL; instant
   offline on clean shutdown and on suspend/shutdown power events.
2. Controller subscribes to presence of **its paired devices only** (the server enforces this
   from signed pairing records).
3. "Connect": the controller dials the target's endpoint id (iroh). The target's
   `incoming_filter` / auth hook rejects any key not in its paired allowlist **before any
   application byte is parsed**; revocation closes live connections immediately.
4. Inside the authenticated QUIC connection the peers exchange `Hello`
   (versions, capabilities, downgrade floor), then the target applies its **consent policy**
   (attended prompt / unattended + step-up factor) and shows the on-screen indicator.
5. Streams open: `control` (RPC), `input` (reliable, high priority), `cursor` (datagrams),
   per-frame `video` uni-streams, `audio` (datagrams), plus `clipboard` / `files` /
   `terminal` / `portfwd` bi-streams on demand.
6. QoS loop (delay-gradient estimator → encoder bitrate / fps / resolution ladder) and
   telemetry overlay run for the lifetime of the session; every session is written to the
   signed, hash-chained audit log.

## 5. Where each requirement is handled

| Requirement | Mechanism | Status |
|---|---|---|
| E2E encryption, forward secrecy | QUIC/TLS 1.3 with pinned Ed25519 identities | ✅ |
| Zero-trust presence | server ACL from dual-signed pairing records | Phase 2 |
| < 50 ms LAN glass-to-glass | per-frame streams, local cursor, adaptive bitrate; HW codecs and zero-copy are **not built** | ✅ 7.9 ms p50 loopback (software); real LAN ⛔ |
| Wake | an always-on agent on the target's LAN (`--wake-helper`) sends magic packets on request | ✅ (no real sleeping machine tried) |
| Windows UAC / lock screen | SYSTEM agent in the console session, threads attached to `Winsta0\Winlogon` | 🟡 written, never run (WINDOWS.md) |
| Auditability | hash-chained, signed append-only log | ✅ |

The live truth of what is implemented and *verified* is in [STATUS.md](STATUS.md).

## Implementation update: 2026-10-12

The native viewer now presents through wgpu (CPU BGRA upload; GPU scaling) with a software fallback.
The agent optionally selects FFmpeg vendor encoders after probing actual immediate H.264 output;
VideoToolbox remains macOS's default hardware path. See ADR 0021 and ACCELERATION.md.

Incoming sessions negotiate step-up before any target handler/resources run. Authenticated local
IPC manages factor enrollment and the controller's ephemeral browser prompt. A successful ceremony
preserves the exact encrypted connection for one next command (60-second expiry), with no reusable
authorization token. See ADR 0020 and STEP_UP.md. Paired devices connect automatically by default; passkey/TOTP step-up is enforced when explicitly enabled.

Privacy requests travel over the existing encrypted control stream and require a `privacy` grant.
A target holds an OS privacy lease and restores it on termination or watchdog/local escape. Windows
normal-desktop shields are implemented; unsupported platforms refuse explicitly. See ADR 0022.
`nexus-relay` now embeds the upstream iroh server with TLS/admission/traffic limits; it never sees
plaintext media/input. See ADR 0023 and the separate Compose relay bundle.

## Windows control completion

The Windows service now brokers secure attention using a SYSTEM-only per-child event and a
session-token-impersonating SendSAS call. Children start suspended, enter a kill-on-close job and
then resume in the selected console or existing active RDP session. Capture/input own their
Windows resources on dedicated desktop-following threads; DXGI reconstruction and GDI fallback
cover display transitions. A separate UI thread handles the target badge and local physical panic.
See [ADR 0024](adr/0024-windows-control-completion.md). This revision was implemented without
running tests, builds, lint, dependency checks or Windows runtime validation, by request.
