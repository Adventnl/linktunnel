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
* **GPU encoders and decoders** (NVENC, AMF, QuickSync, VAAPI, HW decode), zero-copy capture→encode, intra-refresh, reference-frame
  invalidation, video FEC, AV1/HEVC. Today: software OpenH264 everywhere, VideoToolbox encode on Macs (unrun). Consequence: 4K60 and
  120 fps are not realistic yet; 1080p software encode peaks at ~80 fps on a 4-core x86-64 CPU.
* **The wgpu native viewport.** The viewer is `winit` + `softbuffer` (CPU presentation), not a GPU swap-chain: no vsync-bypass "game
  mode", no GPU scaling. It is functional and low-latency in loopback but not the final design (ADR-0012 still states the goal).
* **Privacy screen** (blank the target's physical display and block its local input), **IME-aware** text entry beyond scancodes +
  Unicode injection, pinch-zoom viewport, seamless per-window streaming.
* **Windows secure-desktop control: written, never executed** (no Windows machine). Same for the whole Windows and macOS runtime:
  capture, input, presence sensors, service, installers.
* **Step-up authentication** (WebAuthn passkeys, TOTP) from the spec: designed in ADR-0006, not implemented. Unattended access is a
  per-device switch, not a passkey ceremony.
* **Always-visible session indicator, panic hotkey and "new connection" push/email notifications on the target.** The consent prompt,
  per-device pre-authorisation, signed audit log and lockdown exist; the persistent on-screen indicator does not.
* **Encrypted local session recording, quiet hours, geofence, per-device allowed-controller lists beyond pairing grants.**
* **Horizontal scaling of the signaling server** (presence/envelope routing is per-process: run one replica; state is in PostgreSQL/Redis
  so restarts are safe).
* **`nexus-relay` as a product.** Relays are stock `iroh-relay` (see ADR-0003); we ship configuration guidance, not a custom relay.
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
