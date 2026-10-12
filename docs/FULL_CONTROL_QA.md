# Acceptance target: full live desktop control

The product goal is AnyDesk-style full, authorized control of a PC through live screen relay:
see the current desktop and operate its keyboard, mouse, applications and administrative prompts,
with consent/visibility, E2E encryption, files, clipboard, audio and power/terminal tools.
This checklist must be demonstrated on real machines before calling the product complete.

## Mac controller → Windows target

1. Install the SYSTEM service using WINDOWS.md; verify Screen Recording/Accessibility for the Mac
   if it will also be a target. Pair both machines and compare SAS. Enroll a controller-usable
   passkey or TOTP on the Windows service (`nexus --system auth ...`).
2. `nexus remote view 'Windows PC'`: complete step-up, check live pixels and encoder HUD,
   click/move/drag, type into multiple applications, exercise modifiers and scrolling.
3. Test every monitor at mixed DPI; switch monitors (Ctrl+Alt+Shift+M), fit/pixel
   (Ctrl+Alt+Shift+P) and fullscreen (Ctrl+Alt+Shift+F). Verify pointer alignment.
4. Lock the PC, enter *your own* credentials remotely, open an elevated app, approve a UAC prompt,
   and return to the normal desktop. Repeat during user switching/RDP interactions.
5. With a `privacy` grant, Ctrl+Alt+Shift+B must obscure all physical monitors while the viewer
   continues showing the actual desktop. Test local-input blocking, remote typing/clicks, physical
   Esc restoration, disconnect, lockdown, revocation, agent crash, and stalled lease renewal.
   Verify no shield is applied to a secure desktop. Use a second person/physical observation.
6. Test clipboard opt-ins, audio output, files/hash verification, terminal/port forwarding,
   lock/sleep/restart/shutdown and wake. No extra privilege should be inferred from screen access.
7. Run the hardware encode stream/rate/keyframe test on NVIDIA/AMD/Intel separately. Record actual
   LAN and WAN input-to-photon p50/p95, FPS, bandwidth and resource use, at 1080p and 4K.
8. Force a relay-only path through the deployed `nexus-relay`, then allow direct connectivity;
   verify migration, TLS validation, token admission and maintained device authorization.

## Windows → Mac and Linux

Repeat desktop/input/media tests in reverse. macOS login-window control is OS restricted.
Linux physical-screen privacy and Wayland-native capture/input are not implemented; report that
explicitly. Use private Xvfb for headless GUI servers and verify real Linux desktop input separately.
Fresh unattended transport reconnects currently require a fresh factor; attended resume retains
its existing short approved reconnect window.

## Evidence available from this implementation

Mac: real Metal offscreen shader pixel/orientation/letterbox test; browser WebAuthn registration
and authentication with a temporary test authenticator; TOTP/session binding/replay tests;
real two-agent video/input/files/terminal/forwarding and step-up tests; standalone relay process
with TLS, admission and relay-only traffic. Windows privacy is cross-compiled and its network
lease lifecycle is exercised with a recording OS backend. This does **not** establish Windows
physical privacy, UAC/login control, real monitor capture, zero-copy, 4K60 or WAN latency budgets.
