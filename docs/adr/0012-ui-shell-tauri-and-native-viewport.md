# ADR-0012: UI shell: Tauri + native wgpu viewport

**Status:** Accepted (design; Phase 8)

## Decision
Dashboard, settings and the file manager are a **Tauri 2** app (Rust backend, React +
TypeScript + Tailwind frontend). The **video viewport is a native `wgpu` surface** in its own
window/child surface; **no video ever passes through the web layer**. The frontend talks to the
Rust backend over Tauri commands/events only; the backend talks to the local agent over the
authenticated IPC. Terminal uses xterm.js in the web layer (text only).

Idle budget: < 150 MB RAM for the app, < 80 MB for the agent; verified by a script in `bench/`.
All user-facing strings are localised (en, zh-CN) in both the Rust and TS catalogs, with a CI
parity check on both.

## Windows workspace implementation (2026-11-03)

Windows now parents the native viewer HWND into the dashboard client area. The Rust shell
supervises the existing CLI process; a bounded stdin JSON protocol carries physical-pixel
geometry, scaling and visibility, and stdout reports first-picture readiness, statistics and
session exit. Pixels remain in the native renderer. UI overlays hide the child HWND rather
than attempting to composite web dialogs above native video. Closing during pending consent
interrupts startup; acknowledgement follows process reaping.

The renderer decodes reference-dependent H.264 in order but holds only one pending decoded
picture for presentation. Fill and Actual Pixels share their source-crop math with mouse and
touch mapping. Standalone CLI windows remain supported, as does the standalone fallback on
other desktop operating systems. This changes no pairing, IPC authentication or remote wire protocol.

Verification: `scripts/test-embedded-viewer.ps1` received a real 1920x1080 paired stream and
verified WS_CHILD parentage, resize to 777x444 and hiding. Renderer tests cover crop mapping,
GPU orientation and latest-frame replacement; desktop supervisor tests cover timeout, failure,
close during startup, console suppression and post-ready layout messages.
