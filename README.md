# Nexus Remote

A secure, real-time, cross-platform remote-control suite for devices **you own or are authorised to administer**: screen, keyboard,
mouse, clipboard, audio, files, terminal, port forwarding and power, between Windows, macOS and Linux (desktops and headless servers),
with live device presence, Wake-on-LAN and a dashboard (desktop app, tray, and terminal UI).

> **Project status:** [docs/STATUS.md](docs/STATUS.md) is the honest ledger of what is implemented, what was **verified and on what**, and what
> is not started. The Linux stack is built and tested end to end. macOS has native viewer and two-agent verification;
> Windows now has a native build, real-desktop encrypted capture verification, and an installed boot-time SYSTEM host.
> **MacBook-to-Windows live input, UAC/login and outside-home access still need a two-device acceptance test.**
> Optional hardware encoders are implemented but Windows vendor GPU operation remains unverified.
> Read [docs/LIMITATIONS.md](docs/LIMITATIONS.md) before trusting it with anything important.

## Documents
Android coding companion: [APK setup, workflow and verification](docs/ANDROID_CODEX.md).
Choose a PC repository and send Codex prompts from a native phone interface; no desktop stream needed.

| | |
|---|---|
| [Status](docs/STATUS.md) | per-phase ledger with verification evidence |
| [Limitations](docs/LIMITATIONS.md) | what does not work, and why |
| [User guide](docs/USER_GUIDE.md) | commands, special machines, troubleshooting |
| [Architecture](docs/ARCHITECTURE.md) · [Protocol](docs/PROTOCOL.md) · [ADRs](docs/adr/README.md) | how it is built and why |
| [Threat model](docs/THREAT_MODEL.md) | STRIDE analysis; every mitigation cites its test |
| [Windows](docs/WINDOWS.md) | service, UAC / lock-screen approach, privileges, first-run checklist |
| [Self-hosting](docs/SELF_HOSTING.md) | signaling server: Compose, Helm, systemd |
| [Conventions](docs/CONVENTIONS.md) | lint policy, error handling, unsafe, logging, i18n |

## Try it (Linux; needs `xvfb`, `libasound2-dev`, `cmake`, a C compiler)
```sh
cargo build --workspace
cargo test  --workspace

./scripts/demo-local.sh      # signaling server + two agents + CLI as separate processes
./scripts/demo-server.sh     # a monitor-less "server": virtual screen, terminal, port forward, SSH proxy, file transfer
./scripts/demo-app.sh        # the Tauri desktop app against real agents (needs the Tauri libraries)
```
Then, with two paired devices: `nexus dashboard`, `nexus remote view <device> --audio`, `nexus shell <device>`, `nexus cp`, `nexus forward`,
`nexus proxy` (SSH ProxyCommand), `nexus power`, `nexus wake`. See the user guide.

## Repository layout
```
crates/        Rust workspace members (crate map in docs/ARCHITECTURE.md)
apps/          nexus-app: Tauri 2 desktop app (React + TypeScript), a thin client of the agent
fuzz/          13 cargo-fuzz targets (nightly toolchain)
installers/    systemd units + .deb builder, macOS pkg/launchd, Windows WiX MSI
deploy/        Docker Compose, Helm chart for the signaling server
bench/         latency / bandwidth harness (CI regression gate)
docs/          architecture, ADRs, protocol, threat model, guides
scripts/       demos and checks that double as end-to-end tests
```

## Intended use
For controlling devices you own or are explicitly authorised to administer. Consent, visibility and audit are built in: one-time pairing, automatic access for paired devices, optional per-session approval, lockdown, a signed tamper-evident audit log. It is not designed for covert use and should not be used that way.

## License
Not yet chosen; all crates are `publish = false`. Pick a license before distributing.

### Live control implementation update

The native viewer now uses wgpu; optional FFmpeg backends provide NVENC/AMF/Quick Sync/VAAPI
H.264 encode. Optional passkey/TOTP step-up can be enabled before resource access; when enabled, the viewer
opens a browser prompt automatically. Windows session-scoped privacy and a standalone TLS relay
are implemented. See [setup](docs/STEP_UP.md), [acceleration](docs/ACCELERATION.md),
[self hosting](docs/SELF_HOSTING.md), and [verified status](docs/STATUS.md).
For the MacBook-to-PC setup, follow [Mac to Windows](docs/MAC_TO_WINDOWS.md).
The acceptance target remains full live Mac↔Windows desktop/input control including elevated
Windows/UAC/login sessions. Real Windows secure-desktop and GPU/privacy validation are still needed.
