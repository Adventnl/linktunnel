# Limitations: what does not work, and why

An honest list, kept next to [STATUS.md](STATUS.md). Items are grouped by cause so you can tell *can't* from *haven't yet*.

## Physically or contractually impossible (or not ours to bypass)
* **DRM-protected video** (Netflix, protected Blu-ray players, some banking/enterprise apps) appears **black** over any
  screen-capture API (Desktop Duplication, ScreenCaptureKit, X11). That is the OS honouring the content protection.
* **UAC and the lock screen as a non-SYSTEM process.** Windows hides the secure desktop from user-mode code. We solve it with a
  SYSTEM service ([WINDOWS.md](WINDOWS.md)); there is no way around that requirement and a normal user install cannot do it.
* **macOS login window.** A LaunchAgent only runs inside a logged-in session, so before login nothing can capture or inject.
  Control of the macOS login window needs Apple-restricted entitlements/approaches we do not have; a Mac that has *no one logged in
  is reachable for terminal/files/wake only*.
* **Windows Server multi-session** (more than the licensed RDP sessions) is a licensing matter; nothing here circumvents it.
* **Wake-on-LAN across the Internet.** Magic packets are Layer-2 broadcasts. Waking an off-LAN machine requires an always-on device
  *on its LAN* (`--wake-helper`). WoL from Modern Standby / Wi-Fi depends on NIC, driver and firmware.
* **Hardware-level control (BIOS, power-off machines)** needs vPro/AMT/IPMI/smart-plug drivers: not implemented.

## Not built yet (planned in the brief)
* **Hardware decoding, zero-copy capture→encode, intra-refresh, reference-frame invalidation, video FEC, AV1/HEVC** remain unbuilt.
  NVENC/AMF/Quick Sync/VAAPI H.264 implementations now exist behind `hardware-ffmpeg`, but need the FFmpeg SDK/runtime
  and matching GPU validation. Baseline installers do not bundle FFmpeg. GPU wgpu presentation is implemented and its
  shader was executed on this Mac; decoded BGRA still crosses CPU memory. See [ACCELERATION.md](ACCELERATION.md).
* **Physical privacy on macOS/Linux** remains unbuilt. Windows normal-desktop shields and local-input hooks are implemented
  and cross-compiled, with cleanup verified using a recording backend; they have not run against a real Windows GPU/DXGI
  desktop. Privacy is refused on unsupported platforms/secure desktops. Local Esc restores the screen. See ADR 0022.
* **IME-aware** text entry beyond scancodes + Unicode injection, pinch-zoom viewport and seamless per-window streaming.
* **Windows control completion is source-only, uncompiled and unexecuted for this revision.**
  Native injector selection, service-brokered Ctrl+Alt+Del, RDP/boot/session supervision, capture recovery,
  DPI/rotation/hot-plug, touch/pen, shortcut capture and target badge/panic are implemented. No tests,
  builds, lint or dependency checks were run for this update by request. See ADR 0024.
* **Windows secure-desktop control: written, never executed** (no Windows machine). Same for the whole Windows and macOS runtime:
  capture, input, presence sensors, service, installers.
* **Account login, hardware-key-only policy, factor recovery/removal UI and unattended reconnect without a fresh factor** remain unbuilt.
  Device-local, session-bound WebAuthn/TOTP step-up is implemented and enforced before resources. Browser and two-agent
  verification tests pass. See [STEP_UP.md](STEP_UP.md).
* **macOS/Linux always-on-top session badges and push/email notifications to your other devices.**
  Windows has a separate target badge and physical panic hook in the service child, authored without
  runtime validation; exclusive fullscreen applications can obscure topmost windows. What
  exists: tray/menu and dashboard banners, `nexus sessions`, a desktop notification on each new connection, and a panic hotkey. All of
  these live in the agent/app UI: someone looking only at a full-screen game or video would not see them, and the notification and
  hotkey are untested on real desktops (Wayland may refuse global shortcuts, and a headless server has neither).
* **Encrypted local session recording, quiet hours, geofence, per-device allowed-controller lists beyond pairing grants.**
* **Horizontal scaling of the signaling server** (presence/envelope routing is per-process: run one replica; state is in PostgreSQL/Redis
  so restarts are safe).
* **Production relay operations/WAN validation** remain unverified. `nexus-relay` now ships TLS, admission and traffic limits
  around upstream iroh-relay; the real binary passes an encrypted relay-only/admission test. The Compose bundle is authored, unrun.
* **Mobile apps and a browser controller; session sharing / guest links; multi-user teams.**
* **Crash reporting, auto-update daemon loop** (the signed-update library and CLI exist; nothing polls and applies by itself).
* **Wayland headless compositor and KMS capture.** Headless servers use Xvfb (X11). Wayland desktops: capture goes through X11/Xwayland
  where available; portal/PipeWire capture is not implemented; input uses XTest (not `uinput`), which Wayland compositors may ignore.
* **Linux audio**: capture uses cpal (ALSA), so loopback needs a PulseAudio/PipeWire *monitor* device (and sometimes `PULSE_SOURCE`).
  Devices that cannot run at 48 kHz f32/i16 are refused: there is no resampler.

## Verified only partially (see STATUS.md for the exact evidence)
* Nothing has been measured on a **real WAN or behind real NATs**. Hole-punching success rates and WAN latency numbers are *not known*.
  The latency figures quoted (7.9 ms p50 glass-to-glass) are **loopback with synthetic content and software encoding**.
* GitHub Actions workflows (cross-OS matrix, benchmark gate, nightly fuzz) are written but have **not been run**.
* Fuzzing is a smoke test (≥ 45 s per target, longer for the video and audio decoders), not a campaign.
* No independent security review or penetration test has been done.

## Design limits worth knowing
* **A paired device with the `terminal` grant can run commands** as the agent's user (or SYSTEM/root if you configure that). Grants are
  chosen at pairing time; changing them means re-pairing. The `audio` grant did not exist before this session, so older pairings do not
  carry it.
* **Metadata**: the signaling server learns who is paired with whom and when they are online (self-host to remove that).
* **mDNS discovery** reveals to the LAN that *some* Nexus device is present (not whose, not its name); it can be switched off.
* **The CLI/IPC is only as private as the local account**: same-user malware can use the agent's IPC token. A SYSTEM service's IPC
  is administrator-only.
* **Consent prompts need someone to answer.** A headless machine refuses attended sessions immediately unless a UI client is connected;
  that is deliberate (fail closed). Use per-device pre-authorisation for unattended boxes.
