# ADR-0019: Desktop app as a thin client of the agent

**Status:** Accepted (verified on Linux)

## Decision
`apps/nexus-app` (Tauri 2, React/TypeScript/Tailwind) is a *client of the local agent over its IPC*, like the CLI: it holds no keys and does no
networking. A reconnecting `AgentLink` forwards agent events to the UI on one channel; commands are thin calls. Because the app is a connected IPC
client it also answers attended-consent prompts. **Video never passes through the web layer**: "Connect" starts the native viewer (`nexus remote view`).
The terminal is xterm.js rendering bytes the agent relays. The UI runs unchanged in a plain browser against a mock agent (a `Bridge` interface), which
is how it is designed, tested and screenshotted. Standalone Cargo workspace so the webview toolchain does not touch agent/server builds.

## Consequences
* ✅ Real app + real agents + xdotool-driven terminal on Linux (`scripts/demo-app.sh`); 19 frontend + 5 backend tests; en/zh-CN parity test.
* Not built or run on macOS/Windows; the tray needs an app-indicator host on Linux; closing the window hides it only if a tray was created.
* The `nexus` binary must be next to the app (or on `PATH`) to open the viewer.
* Files are CLI-only for now; per-device policy beyond the pre-authorise switch is not in the UI.
