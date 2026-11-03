# Link Tunnel desktop

A light-glass workspace for paired computers. Select a computer in the sidebar, then open
its desktop or terminal. Windows embeds the native GPU viewer in the workspace, with one
window to move, resize or fullscreen. macOS/Linux retain the standalone native viewer.
Video never passes through React or the webview.

- Right-click a device, press Shift+F10, or use its actions button for power and Unpair.
  Offline devices can still be removed. Removal requires confirmation and revokes local access.
- Fit shows the whole screen. Fill crops edges without distorting the picture; Actual Pixels
  shows a centered crop at native pixel size. Rendering and pointer mapping share that crop.
- Quality preference applies to the next connection: Automatic, Sharper text (30 fps,
  20 Mbit/s starting budget), or Smoother motion (60 fps, 30 Mbit/s starting budget).
  Network adaptation remains active; these are preferences, not promised throughput.
- FPS, received bitrate and latency appear in the connected workspace footer.
- Native video hides behind dialogs, device menus, other devices and the terminal tab.
  Disconnect cancels pending startup as well as stopping an established viewer.
- The command palette is optional (Ctrl+K / Command+K). Settings retain light/dark/system
  appearance, English/Simplified Chinese, access controls and offline removal.
- Phone control uses separate local adb/scrcpy authorization. Its native window and Stop
  lifecycle remain separate from agent pairing and lockdown. See [phone control](../../docs/PHONE_CONTROL.md).

The local agent must be installed and running. Pairing needs a reachable configured signaling
server. The UI reports an unavailable agent/server and recovers when it reconnects.

## Commands without the dashboard

The bundled `nexus` executable is the CLI. On an installed Windows host, use `--system`:

```sh
nexus --system devices --json
nexus --system status --json
nexus --system remote view MY-PC --scale fill --fullscreen --fps 60 --mbps 30
nexus --system shell MY-PC
nexus phone devices
nexus phone pair 192.168.1.2:35000  # prompts privately for the pairing code
nexus phone view SERIAL --screen-off --fps 60
nexus --help
```

File copy, port forwarding, identity, pairing, consent, trust, power, wake, audit, updates,
lockdown and incoming-session disconnection are also available through the CLI.
See the [user guide](../../docs/USER_GUIDE.md) for the command reference.

## Source and checks

`src/App.tsx` composes the shell and dialogs. `src/hooks/` owns app control and native viewport
synchronization; components render the workspace. `src-tauri/src/workspace.rs` owns native
session commands; `viewer.rs` supervises the process. `crates/nexus-render` owns decode,
one-slot presentation, GPU scaling and remote input; `crates/nexus-cli` owns CLI operations.

```sh
npm ci
npm run dev             # clearly labelled browser preview; cannot control a real desktop
npm test
npm run test:workspace
npm run test:pairing
npm run test:phone
npm run build
npm run screenshots     # .impeccable/review/ at the repository root
npx tauri dev
npx tauri build
```

On Windows, dot-source `scripts/windows-build-env.ps1` before native Cargo builds.
`installers/windows/build-desktop.ps1` builds the Windows installer with agent and viewer.
`scripts/test-embedded-viewer.ps1 -Device DEVICE` checks a real paired stream in a hidden
native parent, including resize and overlay hiding, with remote input disabled.

The browser preview and UI tests simulate the native boundary. They do not measure WAN
performance or prove a hardware encoder. See [acceleration limits](../../docs/ACCELERATION.md).
