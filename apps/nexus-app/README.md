# Nexus Remote desktop app

The dashboard window and tray icon: live status dots for every paired device, one-click
**Connect / Terminal / Wake / Lock / Sleep / Restart / Shut down**, a command palette (⌘K / Ctrl+K),
the pairing wizard (code + four-word comparison), attended-access consent prompts, per-device
"pre-authorise" switches, a lockdown switch, English and 简体中文, light/dark/automatic theme.

```
 React + TypeScript + Tailwind (src/)         Rust (src-tauri/)
 ┌──────────────────────────────┐   invoke   ┌────────────────────────────┐   IPC    ┌─────────────┐
 │ dashboard, palette, dialogs, │ ─────────▶ │ thin commands + tray       │ ───────▶ │ nexus-agent │
 │ xterm.js terminal            │ ◀───────── │ events from the agent      │ ◀─────── │ (local)     │
 └──────────────────────────────┘ nexus://   └────────────────────────────┘ Unix     └─────────────┘
                                   event                                      socket /
                                                                              named pipe
```

* The app holds **no keys and does no networking**. Everything goes through the local agent, exactly like the
  `nexus` CLI. If no agent is running the window says so and reconnects on its own.
* **Video never passes through the web layer.** "Connect" launches the native viewer (`nexus remote view`,
  a wgpu/winit window with local cursor and low-latency presentation). The terminal *is* in the web layer: xterm.js
  rendering bytes the agent relays over the encrypted session.
* With a tray icon present, closing the window hides it; the tray menu lists devices with their status dots and connects on click.

## Develop
```sh
npm install
npm run dev          # the UI in a browser with a built-in mock agent (no Tauri needed)
npm test             # state, palette, i18n parity, encoding
npm run typecheck
npm run build && npm run screenshots   # headless-Chromium screenshots into ./shots

# the real app (needs the Tauri system libraries; see https://tauri.app/start/prerequisites/)
npx tauri dev
npx tauri build      # .deb / AppImage / .msi / .nsis / .app / .dmg for the host platform
```
On Linux the system libraries are `libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev`.

## Verified
`scripts/demo-app.sh` builds the app, starts a real signaling server and two real agents, runs the app on a virtual X
display, screenshots the live dashboard, then **drives the GUI with xdotool to open a terminal on the paired server and
run a command**, which lands on the server through the encrypted session. See docs/STATUS.md for what was and was not run.
