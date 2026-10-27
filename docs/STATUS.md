# Status ledger

The honest record of what exists and **what was verified, on what**. Nothing elsewhere in this repository
claims a capability this page does not.

## Desktop-to-Android phone control — 2026-10-27 (Windows development)

Added the desktop **Phone** panel and `nexus-phone` backend for local adb/scrcpy
control: USB discovery, Android 11+ wireless pairing/connect/disconnect, exact
device selection, physical-display-off mode, optional media audio, resolution/fps
options, native viewer supervision, stop and desktop-exit cleanup. This transport
uses Android authorisation independently of Nexus agent pairing and lockdown.

Verified on Windows: 13 backend tests (including real subprocess failure/timeout
and cancellation checks), strict crate/native clippy, native Tauri `cargo check`, 9 native unit tests, 25
frontend tests, frontend production build, and Chromium UI/bridge integration
checks with simulated native replies for Windows, Mac and missing-tool cases.

No Redmi was attached. Actual USB/Wi-Fi control, HyperOS permissions, screen-off
behaviour, measured smoothness, media audio and Mac native execution are **not
hardware-verified**. Windows cellular calling opens external Phone Link setup;
Bluetooth transfer and mic/headphone routing remain unverified. Native Nexus
call-audio routing, Mac Android call handoff, phone internet relay and powered-off
phone wake are not implemented. See [setup and acceptance](PHONE_CONTROL.md).

## Android Codex companion and Windows wake capture — 2026-10-27 (Windows)

Implemented a native Android repo/chat client and a separate user-session Codex host over
certificate-pinned HTTPS/Tailscale. Supports prompts, progress, diffs, clarification questions,
individual command/file approvals, stop, follow-ups and bounded persisted history. Built the
development APK and verified v2/v3 signing. Thirteen host tests pass on Windows, including a
power-lease lifecycle check. Active tasks prevent idle system sleep without waking the display.
A real task through the installed Codex CLI created a file in a disposable workspace and
edited it in a follow-up, producing streamed agent replies and completed status. The configured
host listens on this PC's Tailscale address; HTTPS/authentication/repository listing passed
locally. Current-user logon startup is installed. Pairing material stays outside the repo.

Windows capture now requests display activity while streaming and rechecks an empty initial
surface during DXGI timeouts, preserving protected-content behavior. Eight capture unit tests
pass, including two new recovery-state tests; live capture of the awake 1920×1080 desktop
and cursor passes. Strict capture Clippy and changed-module
formatting pass. Whole-workspace rustfmt reports pre-existing CRLF files across the checkout.
The locked release build of `nexus-agent` and `nexus-cli` completed successfully on Windows.

Pending: administrator installation of the repaired desktop host and a Tailscale-only firewall
rule; Android device installation/UI/phone-network acceptance (no attached ADB device);
live Mac-to-PC wake/picture/UAC acceptance. Separate mobile conversations edit the selected
PC working tree; this does not attach to the active VS Code conversation. See
[Android setup, plan and limits](ANDROID_CODEX.md) and [ADR 0025](adr/0025-android-codex-companion.md).

**Legend**
✅ implemented **and verified here** by automated tests or a scripted run on the build machine ·
🟡 implemented and type-checked / linted for the target, **not executed** there ·
🔲 not started ·
⛔ cannot be verified from the available environment (reason given)

## Readable desktop and roaming update — 2026-10-19 (Apple Silicon Mac)

The controller now requests native-resolution video and adapts fps, with hysteresis, to keep
text detail on limited connections. It uses the existing `SetQuality` message understood by
the installed Windows host. Late older per-frame streams no longer reset the delay estimator's
baseline; bandwidth recovery at low fps uses elapsed time. Changed transport addresses are
republished on the activity check (normally 10 seconds) after moving between networks.

Verified locally: 48 media/agent unit tests, 9 media integration tests (including a decoded
1280×720 native picture at 500 kbit/s and 5 fps), 24 agent integration tests, strict Clippy for
both changed crates/all targets, formatting, and a locked optimized agent/CLI build. Both
network adaptation tests pass when run serially with the compiler stopped: 40 Mbit/s shaped
path delivered 9.47 Mbit/s with 47 ms p95 capture-to-receive latency; on a 4 Mbit/s shaped path,
adaptation reduced p95 from 872 ms to 348 ms. The first stress-test run during concurrent
compilation failed its performance thresholds; those timings are not a WAN measurement.

The optimized agent was installed with the previous binary backed up under
`~/Library/Application Support/Nexus Controller/backup-before-readability-*`.
The user approved macOS Keychain access. The updated LaunchAgent is running and responds
to local IPC: device ID and fingerprint match the pre-update values, one paired device is
retained, and the pairing store's SHA-256 is unchanged. The installed binary matches the
tested optimized build. The VPN address is advertised; signaling continues to retry while
the Windows host is offline.

The Mac's Tailscale backend is healthy and retains the PC's VPN signaling address. The
Windows PC is offline. Physical Wi-Fi quality at distance, PC service/firewall state, and
live control from a separate network in New Zealand remain unverified.

## MacBook controller setup — 2026-10-15 (macOS 26.6.2, Apple Silicon)

Tailscale 1.104.1 was already installed; onboarding and sign-in to the PC's tailnet completed.
The Mac is `100.80.114.60`; the Windows peer is `DESKTOP-H9VGBNO`, `100.114.106.93`.
Initial Mac-to-PC checks passed: Tailscale ping (3 ms over the home LAN), TCP port 8443,
and `GET /healthz` (`200 OK`, `ok`). Tailscale was added to macOS **Open at Login** using
System Settings after its own startup toggle reported an error. Actual logout/reboot startup
has not been tested.

`cargo build --release --locked -p nexus-agent -p nexus-cli` passed at revision `b70bbda`.
Binaries are installed under `~/Library/Application Support/Nexus Controller/bin`, with
`~/.local/bin/nexus` and `~/.local/bin/tailscale` CLI links. The per-user LaunchAgent
`~/Library/LaunchAgents/app.nexus-remote.controller.plist` is loaded and running, using
`--name MacBook --no-screen --no-terminal` and the VPN signaling URL. Its RunAtLoad and
restart-after-failure settings are configured; login/crash recovery remains untested.
The agent created its identity in macOS Keychain; `nexus status` shows zero pairings/sessions.
Logs are under `~/Library/Logs/Nexus Controller`.

At the final connectivity check, the Mac's Tailscale backend was healthy/online but the
Windows peer was **offline** (last seen 21:40 NZDT); ping and port 8443 then timed out and
Nexus signaling was disconnected. On recheck at 21:48 NZDT, macOS Keychain approval had
cleared and CLI `doctor` completed successfully: config, state, secret store, identity and
agent checks passed, with signaling unreachable reported as a note. The Windows peer was
still offline and both VPN ping and port 8443 timed out. Restore Windows connectivity
before continuing. There is no reachable Windows administration
session from this Mac, so initial `nexus --system pair offer` must run locally on the PC.

**Still pending:** pairing and verification-word comparison, authentication enrollment,
attended/unattended live control, UAC, Windows login, audio/input, reboot recovery, and
phone-hotspot access. The initial VPN health check is not proof of outside-home control.

## Windows host installation — 2026-10-15 (native Windows 10 x64)

The Windows PC now has a fresh locked release build of `nexus-agent`, `nexus`, and `nexus-signal`,
plus the previously built native desktop dashboard. `scripts/check-windows.ps1` completed
successfully: strict workspace/all-targets Clippy with warnings denied, and 563 Rust tests
passed, zero failed, two ignored. The ignored tests require an interactive physical desktop
and the real OS credential store; the separate desktop smoke test below exercised capture.
Evidence: `target/windows-final-checks.log`. The two-agent physical-desktop smoke test
passed again: pairing, monitor discovery, a nonblank 1920×1080 screenshot over encrypted QUIC,
direct transport, and signed audit verification. This is a same-PC transport test, not a Mac/LAN
latency measurement. Evidence is under `target/windows-smoke-562edfbbb66d4f32b746af61c9e74f99`.

`install-host.ps1 -LanAddress 192.168.1.32 -KeepAwakeOnAc` completed in an elevated process.
Verified: `NexusAgent` is Automatic/LocalSystem and running; service/session-child processes
exist in sessions 0 and 4; the persistent signaling server listens on the PC's LAN address;
installed `nexus --system doctor` passes and signaling is connected. Installed state is under
`C:\ProgramData\Nexus`; the installer configures local-subnet firewall rules and saves the old
sleep settings before disabling timed sleep/hibernation on AC power.
Evidence: `target/host-install-transcript.txt` and `target/host-install-result.txt` (`SUCCESS`).

The installed host has no paired controller or enrolled authentication factor yet.
`unattended_requires_stepup` remains enabled. MacBook pairing/factor enrollment, actual remote
input, UAC/login, audio, physical privacy, reboot recovery, and separate-network connectivity
remain pending. See [MAC_TO_WINDOWS.md](MAC_TO_WINDOWS.md); the historical rows below are not
claims that these acceptance checks passed.

## Windows control source update — 2026-10-12

Implementation added for native Windows backend selection, service-mediated Ctrl+Alt+Del, boot/session
retry and RDP selection, kill-on-close child jobs, strict state-tree ACLs, capture recovery/GDI fallback,
monitor rotation/hot-plug and mixed-DPI coordinates, native viewer system shortcuts, synthetic touch/pen
and contact release, target session badge/physical panic, and privacy desktop/monitor cleanup.
MSVC binaries now embed a DPI/long-path manifest; MSI authors the default SAS policy only if absent.
The Windows MSI build script offers `-HardwareFfmpeg` when the operator supplies the FFmpeg SDK/runtime.

**No tests, builds, lint, dependency checks or Windows runtime validation were run for this update,
as explicitly requested.** Earlier counts below describe earlier revisions; they do not validate
these source changes. See [WINDOWS.md](WINDOWS.md) and [ADR 0024](adr/0024-windows-control-completion.md).

## Implementation update — 2026-10-12 (macOS Apple Silicon)

* ✅ Native wgpu viewer: Metal shader executed and checked for BGRA colour order, orientation,
  opaque alpha and letterboxing; software fallback retained. CPU decoding/upload remains.
* 🟡 NVENC/AMF/Quick Sync/VAAPI: real FFmpeg backends compile and pass strict lint; device probe,
  low-delay H.264 output and rate-change code implemented behind `hardware-ffmpeg`. Matching
  Windows/Linux vendor GPUs are unavailable here. Hardware decoding and zero-copy remain unbuilt.
* ✅ WebAuthn/TOTP: actual Chrome browser registration/authentication with a temporary virtual
  authenticator; verifier expiry/binding/replay/persistence tests; two real agents refuse resource
  access before step-up and reuse the approved exact connection once. Viewer prompts automatically.
  `unattended_requires_stepup` is enforced when enabled. Personal hosts now default to paired access without recurring prompts; explicit approval/factor settings are preserved.
* 🟡 Windows physical privacy: cross-target compile/lint; ✅ encrypted request/consent/grant and
  lease restoration tests with a recording OS backend. No Windows physical-screen test yet.
  macOS/Linux physical privacy remains unsupported.
* ✅ Standalone `nexus-relay`: actual binary, pinned TLS, relay-only encrypted traffic and denied
  token test; Compose bundle authored (not deployed). No WAN/NAT performance assertion.
* ✅ All 22 agent end-to-end tests passed on this Mac, including synthetic live video/input,
  files, terminals, forwarding, consent, revocation, disconnect and step-up. **583 workspace Rust
  tests pass**, strict workspace/FFmpeg/Windows-privacy lint and cargo-deny are clean; frontend
  20 tests, typecheck and build pass. Synthetic noninteractive demo scripts explicitly disable
  step-up only in their isolated temporary target configuration. Tauri backend testing is unavailable (locked dependency absent
  from cache; registry DNS failure). Existing transport/resource behavior now includes IPv4 fallback
  after an IPv6-first localhost resolution and correct deliberate-disconnect status.

Measurement and validation details: [Mac verification report](benchmarks/2026-10-12-macos-live-control.md).

Setup: [STEP_UP.md](STEP_UP.md), [ACCELERATION.md](ACCELERATION.md), [SELF_HOSTING.md](SELF_HOSTING.md).
The remaining product acceptance gate is [FULL_CONTROL_QA.md](FULL_CONTROL_QA.md), particularly
Mac→Windows UAC/login, vendor GPUs, physical privacy and measured real-LAN/WAN control latency.

The ledger below describes the **earlier implementation and its original evidence**; this update
supersedes its unbuilt rows for these five components without changing the historical measurements.

**Verification environments.** The Phase 1-2 evidence in the earlier version of this ledger (Keychain store, real
PostgreSQL/Redis containers, relay tests) came from a MacBook (macOS 26, Apple M4). **Everything built since then, including
the whole media/control stack, ran in Linux x86-64 cloud containers** (Ubuntu 24.04, 4 cores, no GPU, no audio hardware, Xvfb
for X11, Chromium, no Docker daemon) with the Windows and macOS targets installed for type-checking only. So **no macOS or
Windows code written after Phase 2 has ever been executed**, and no second physical machine or real NAT was available
at any point. A ✅ row names its evidence and runs on Linux unless it says otherwise.

**Totals at the last full run (Linux):** 591 Rust tests pass (`cargo test --workspace`), 8 app-backend tests,
20 frontend tests; `cargo clippy --workspace --all-targets -- -D warnings`, `cargo deny check` clean; 13 fuzz
targets run (see Phase 9). The GitHub Actions workflows are written but **have not been run** (no runner here).

---

## Phase 1: Foundations: ✅
Workspace, pinned toolchain, lint policy (`unsafe` denied except documented FFI, no `unwrap`/`panic` in non-test
code), config layering, tracing, HDR-histogram latency telemetry, en + zh-CN message catalogs with a parity test,
protobuf schemas + hot-path binary codecs with property tests, Ed25519+X25519 device identity with OS-keychain /
owner-only-file stores (known-answer test cross-checked against an independent Python implementation).
Evidence: unit + property tests, `cargo deny`, `cargo audit`. Windows Credential Manager and Linux Secret Service
stores are 🟡 (type-checked; the Linux Secret Service store cannot be exercised in a container without a keyring).

## Phase 2: Networking and security spine: ✅ (one machine) / ⛔ (real NAT)
Noise `XXpsk0` pairing with a six-word code and four-word SAS, dual-signed pairing records, revocations;
`nexus-signal` (axum) with key-possession login, presence, pairing registry, sealed-envelope routing, rate limits;
SQLite/PostgreSQL/Redis stores behind one conformance suite (run against real PostgreSQL 17 and Redis 8 containers
on the Mac); QUIC transport via iroh keyed by device identity, allow-list before any app byte, live revocation,
LAN/direct/relay classification, relay→direct migration against a real local `iroh-relay`.
**New in this session:** **mDNS LAN discovery** ✅ (`nexus-discovery`): privacy-preserving (hourly-rotating tag, no
names, ADR-0017); two real agents with **no signaling server** find each other and exchange traffic
(`paired_devices_find_each_other_on_the_lan_without_any_server`, Linux, multicast worked in the container).
⛔ Hole punching / symmetric NAT on real networks: relied on from iroh, not measurable here. Horizontal scaling of
`nexus-signal` 🔲 (run one replica; documented in the Helm chart).

## Phase 3: First pixels: ✅ Linux X11 ⇄ Linux X11 / 🟡 macOS and Windows backends
Capture (X11 + XDamage dirty rects ✅; ScreenCaptureKit 🟡; DXGI Desktop Duplication 🟡), OpenH264 software codec ✅,
VideoToolbox encoder 🟡 (type-checked for both Mac targets, tests only run on a Mac), per-frame QUIC streams reset when stale,
local cursor stream, input injection (XTest ✅; CGEvent 🟡; SendInput 🟡), viewer window
(`nexus remote view`, winit + softbuffer) ✅, `nexus remote screenshot/type/key/click/monitors` ✅ end to end through two
real agents (`scripts/demo-remote.sh`, Linux).
Earlier viewer used `softbuffer`; the 2026-10-12 update above implements wgpu.

## Phase 4: Hardware acceleration and the latency war: 🟡 partial
Adaptive bitrate (delay-gradient + AIMD) with quality ladder ✅ (tests + shaped-link benchmark: on a 4 Mbit/s,
25 ms link the adaptive controller cut p50 latency from 859 ms to 114 ms, `docs/benchmarks/latest.json`),
dirty-rect skipping ✅, live encoder rate changes ✅, latency telemetry ✅, benchmark harness + CI regression gate ✅
(script; CI job unrun).
**Measured (Linux x86-64, software OpenH264, loopback, synthetic content):** glass-to-glass **p50 7.9 ms, p95 10.8 ms**
(encode 5.5 ms, network 0.2 ms, decode 1.1 ms). 1080p software encode p50 12.4 ms (81 fps max).
🟡 NVENC / AMF / QuickSync / VAAPI encoders now implemented behind FFmpeg (update above). 🔲 Hardware decode, zero-copy GPU paths, intra-refresh, reference-frame
invalidation, FEC for video: **not built**. Software H.264 only (+ VideoToolbox on Mac). The "≤ 40 ms LAN p50" target
is met in the synthetic loopback measurement; real-LAN / real-desktop numbers are ⛔ (no second machine) and the WAN
numbers are ⛔ (no WAN).

## Phase 5: Full control feature set: 🟡 mostly
| Item | State | Evidence |
|---|---|---|
| Clipboard (text + images, per-direction opt-in, not sent on connect) | ✅ Linux | `without_the_clipboard_grant_nothing_crosses`, `text_and_images_roundtrip_through_the_x_clipboard` |
| File panel (resumable, BLAKE3-verified, sandboxed shared folders, symlink-escape refused) | ✅ | `an_interrupted_upload_resumes`, `symlinks_pointing_out_of_the_share_are_refused` |
| Multi-monitor selection / mid-session switch | ✅ | e2e monitor-switch tests |
| Mac↔PC modifier remap, scroll fidelity, HID keymaps | ✅ (unit) | remap/scroll tests |
| Power actions + Wake-on-LAN (direct, or relayed through an always-on device) | ✅ | `a_granted_request_is_performed_after_the_reply`, `wake::tests::a_packet_goes_out_on_the_wire` |
| **Audio streaming (new)**: Opus 48 kHz, FEC + loss concealment, adaptive jitter buffer, mute/bitrate/loss feedback, `audio` grant | ✅ pipeline & transport / 🟡 devices | 19 `nexus-audio` tests; QUIC e2e `tone_arrives_and_is_audible`; fuzzed. **Device capture/playback (cpal: WASAPI loopback, CoreAudio loopback, PulseAudio/PipeWire monitor) is type-checked on Linux but never ran against real sound hardware** (the container has none); macOS ScreenCaptureKit audio is not used (cpal's CoreAudio loopback needs macOS 14.6+) |
| **Auto-reconnect and session resume (new)**: the viewer reconnects after a lost link (1 s→8 s back-off, 5 min), the target skips the attended prompt for a device whose approved session *dropped* in the last 2 minutes (never after a deliberate disconnect), audit-logged | ✅ | `a_lost_link_is_retried_and_a_deliberate_end_is_not` (fake agent speaking the real IPC), `a_dropped_link_may_resume_briefly_and_a_deliberate_end_may_not`, and `scripts/demo-resume.sh`: two real agents, attended, the controller agent SIGKILLed mid-terminal, same persistent shell re-attached with **no second prompt** (and a clean stop *does* ask again). QUIC connection *migration* (IP change without a drop) is quinn's and was not separately tested |
| Privacy screen (blank target display, block local input) | 🟡 Windows | 2026-10-12 update; cross-compiled; recording-backend cleanup tests; real Windows pending |
| IME / international layouts beyond scancode + Unicode text injection | 🔲 | scancodes and `Text` events only |
| Pinch-zoom / gesture viewport | 🔲 | |

## Phase 6: Windows deep control: 🟡 written, ⛔ never run
New this session. `nexus-desktop` (follow the input desktop `Default ⇄ Winlogon`), `nexus-winservice` (SCM service
launching the agent as SYSTEM into the active console session; WTS session-change supervision; `WTSSendMessage`
consent prompts; ACL lockdown of `C:\ProgramData\Nexus`), capture and injection threads re-attach to the input desktop,
agent flags (`--windows-service`, `--no-terminal`), WiX MSI, design in [WINDOWS.md](WINDOWS.md).
Evidence: ✅ the supervisor state machine (13 tests) and command-line quoting (3 tests) run on Linux;
🟡 all Win32 code is `cargo check` + `clippy -D warnings` clean for `x86_64-pc-windows-msvc`; the full `nexus-agent`
could not be cross-compiled here (C dependencies need the MSVC toolchain): the `windows-latest` CI jobs will be its first compile.
**The Phase 6 DoD (unlock the PC at the login screen from the Mac, approve a UAC prompt, control the elevated
desktop) has not been demonstrated.** [WINDOWS.md](WINDOWS.md) ends with the checklist that would turn this green.

## Phase 7: Servers: ✅ Linux / 🟡 Windows Server
`nexus-vdisplay`: supervised **Xvfb virtual display** for machines with no monitor ✅ (5 tests incl. real capture and
input on it; `[display]` config; the agent creates one by itself when no screen is reachable); persistent PTY terminals ✅;
TCP port forwarding ✅; resumable file panel ✅; **SSH ProxyCommand bridge** `nexus proxy` ✅ (half-close aware);
systemd units ✅ verified with `systemd-analyze verify` / `security` (exposure 3.2 "OK"; not run under a live systemd).
**DoD demonstrated on Linux by `scripts/demo-server.sh`** (also a CI job): a monitor-less, DISPLAY-less server agent
creates its own screen; the controller screenshots it (1920×1080), runs a command in a named persistent terminal,
forwards a loopback-only service port, reaches the same service through the stdio proxy, and round-trips a 3 MB file
byte-identically, all through one encrypted session.
🔲 Server Core / Windows Server multi-session; 🔲 headless Wayland compositor (only Xvfb); 🔲 KMS/DRM capture.

## Phase 8: Presence, wake and the dashboard: ✅ mostly
Presence state machine from real OS signals (idle / locked / in use / battery / sleep): the pure classifier and the Linux sensors
(X11 screensaver, logind, sysfs) ✅; macOS (`ioreg`/`pmset`) and Windows (`GetLastInputInfo`, input desktop, power status) 🟡,
instant offline + sleep announcements ✅, Wake-on-LAN direct and via relay device ✅, **wake helper mode**
`nexus-agent --wake-helper` ✅ (config + e2e of the relay path; no sleeping machine was available to wake),
mDNS discovery ✅, **`nexus dashboard`** terminal UI ✅ (status dots, connect/terminal/files/wake/lock/sleep/restart/
shutdown, ⌘K palette, wake-and-connect with progress, en + zh-CN; 15 logic/render tests; driven on a pty in
`demo-server.sh`), and the **Tauri 2 desktop app** ✅ (`apps/nexus-app`): dashboard, palette, pairing wizard, consent
dialog, xterm.js terminal, settings, tray with live status. **Verified by `scripts/demo-app.sh`: the real app on a
virtual X display against two real agents; a GUI-driven terminal command landed on the server.**
Not done: the app has **not** been built or run on macOS or Windows (only Linux/WebKitGTK); tray behaviour without an
`appindicator` host is untested; MSIX 🔲; the in-app file manager 🔲 (files are CLI-only: `nexus ls/cp`).

## Phase 9: Hardening and security review: 🟡 partial
✅ Tamper-evident signed hash-chained **audit log** (chain, signed head, rotation tests); session rate limit and approval
lockout; persistent **lockdown** switch; attended consent with per-device pre-authorisation; signed, staged, rollback-able
**updates** (`nexus-update`: signature, expiry, rollout %, rollback tests).
✅ **Fuzzing (new):** 13 `cargo-fuzz` targets (`fuzz/`): wire formats, every protobuf message, frame decoder, audio
player (libopus), jitter buffer, **H.264 decoder (OpenH264)**, crypto inputs, trust records, update manifests, file-path
sandbox, clipboard/PNG, config TOML, input stream. Each run ≥ 45 s here (h264 400 s, audio 400 s): **no crash in any product
code**; one failing run was a wrong invariant in my harness (re-sent duplicate after a re-sync), fixed and documented in
the target. This is a smoke run, not a campaign: a nightly CI job runs each for 4 minutes.
✅ **Who is connected, and cut them off (new):** the agent tracks live incoming sessions (`ListSessions`, pushed `SessionsChanged`);
`nexus sessions`, `nexus disconnect [device]`, a red banner + `D` key in `nexus dashboard`, a banner / tray "Disconnect everyone" /
tooltip in the desktop app, a desktop notification when a new session starts, and a global **panic hotkey** (Ctrl+Alt+Shift+L:
lockdown on, then disconnect everyone). A local disconnect closes with a deliberate code, clears the resume window and is audited
(`session.disconnected_locally`). Evidence: e2e `the_person_at_a_machine_can_see_and_end_who_is_connected`, dashboard banner test,
app `alert`/`tray`/`dto` unit tests. **Not verified:** the notification and the global hotkey have only been type-checked and
unit-tested for their logic; no notification daemon or hotkey-capable desktop was available (Wayland compositors may refuse global
shortcuts). `nexus doctor` now also checks the agent, signaling, paired devices, lockdown and display availability.
✅ Fixed while there: `NEXUS_HOME` (the documented relocation variable) made config loading fail with "unknown field `home`"; it is
now excluded from the settings environment (`nexus_home_relocates_state_and_is_not_a_setting`).
✅ Threat model v2 ([THREAT_MODEL.md](THREAT_MODEL.md), citations checked by `scripts/check-threat-model.sh`).
🔲 Independent penetration test; privilege-separation audit of the macOS helper; WebAuthn/TOTP step-up (designed in ADR-0006,
not implemented); an always-on-top **on-screen border/badge** while a session is active (see the ✅ below for what does exist);
push/email notification on new connections; per-device quiet hours / geofence; encrypted
local session recording; certificate pinning of the signaling TLS beyond the platform verifier; reproducible builds.

## Phase 10: Packaging, updates and polish: 🟡 written, mostly unrun
| Artifact | State |
|---|---|
| systemd units (agent, desktop user unit, wake helper, signal), hardened | ✅ verified with `systemd-analyze` |
| `.deb` builder (`installers/linux/build-deb.sh`) | ✅ built (release), installed with `dpkg -i` in the container: binaries in `/usr/bin`, `nexus` user created by sysusers, units verify clean; the installed agent ran as the unprivileged `nexus` user (virtual display included) and `sudo nexus --system status` reached it. This found and fixed a real bug: an agent with no relay configured refused to start when the system CA bundle was unreadable (regression test `the_agent_starts_without_readable_ca_roots_when_no_relay_is_configured`). **Not run under a live systemd.** |
| Dockerfile + Compose (signal + PostgreSQL + Redis + Caddy) | 🟡 image builds/starts (Mac); not run in the container (no Docker) |
| **Helm chart** (`deploy/helm/nexus-signal`) | ✅ `helm lint` clean and renders (Helm built from source in the container); not installed on a cluster |
| macOS `.pkg` builder, LaunchAgent/Daemon plists, entitlements | 🟡 plists parse; scripts `bash -n` clean; **never run on a Mac** (needs signing identity, notarization) |
| Windows MSI (WiX v4) + signing script | 🟡 XML well-formed; **never built** |
| RPM, AppImage (agent) | 🔲 (the desktop app's Tauri bundler can emit AppImage/deb; unrun) |
| Update client + signed manifests | ✅ library + `nexus-release`; no update server deployed, no auto-apply daemon loop |
| Crash reporting (opt-in, scrubbed) | 🔲 |
| User guide, self-hosting, troubleshooting | 🟡 written, partly stale before this session; refreshed (USER_GUIDE.md, SELF_HOSTING.md) |
| Clean-machine install < 5 min + automatic connection test | ⛔ no clean machines |

## Not started / out of scope so far
`nexus-relay` as its own binary (relays today are stock `iroh-relay`; ADR-0003) · iOS/Android/web controller · session sharing
with expiring guest links · multi-user teams · smart-plug/IPMI/AMT wake drivers · AV1 / HEVC · Wayland virtual-pointer fast path.

## How to re-verify everything claimed above
```sh
cargo test --workspace                                   # 591 tests (Linux needs xvfb, libasound2-dev, cmake)
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
cargo check --target x86_64-pc-windows-msvc -p nexus-capture -p nexus-input -p nexus-desktop -p nexus-winservice
./scripts/demo-server.sh          # Phase 7 DoD on a headless "server"; also the dashboard TUI on a pty
./scripts/demo-app.sh             # the Tauri app against real agents, GUI-driven terminal
./scripts/fuzz-smoke.sh 45        # all fuzz targets (nightly toolchain + cargo-fuzz)
(cd apps/nexus-app && npm ci && npm test && npm run build)
helm lint deploy/helm/nexus-signal
systemd-analyze verify installers/linux/systemd/nexus-agent.service
installers/linux/build-deb.sh --profile release
```
