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
