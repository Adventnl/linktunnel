# User guide

> What works today, with the exact commands. [STATUS.md](STATUS.md) says what was verified and on what;
> [LIMITATIONS.md](LIMITATIONS.md) says what does not work and why. **Most of this was exercised on Linux only**;
> macOS and Windows instructions are marked where they are untested.

## 1. Install
| Platform | How |
|---|---|
| Any | `cargo build --release` → `target/release/{nexus,nexus-agent,nexus-signal}` (Linux also needs `libasound2-dev`, `cmake`; a C compiler everywhere) |
| Debian/Ubuntu | `installers/linux/build-deb.sh` → `target/deb/nexus-remote_*.deb` (agent + CLI + systemd units) and `nexus-signal_*.deb` |
| Windows | `installers\windows\build-msi.ps1` (WiX v4) → installs the service; see [WINDOWS.md](WINDOWS.md). **Untested on Windows.** |
| macOS | `installers/macos/build-pkg.sh`. **Untested on a Mac.** |
| Desktop app | `cd apps/nexus-app && npm ci && npx tauri build` (see its README) |

## 2. Try it on one machine
```sh
./scripts/demo-local.sh      # signaling server + two agents + CLI, separate processes
./scripts/demo-server.sh     # a headless "server": virtual screen, terminal, port forward, SSH proxy, files
./scripts/demo-app.sh        # the desktop app against two real agents
```

## 3. Use it on two machines
1. Run a signaling server both can reach ([SELF_HOSTING.md](SELF_HOSTING.md)), **or skip it** on one LAN: with
   `lan_discovery` on (the default) paired devices find each other over mDNS. (Pairing itself needs the server,
   because it uses the server as a rendezvous point.)
2. On **each** device: `nexus-agent --signal-url wss://<host>/v1/ws`. For a trial on a *trusted LAN* without TLS add
   `--allow-insecure-signal` and use `ws://…`.
3. Device **B**: `nexus pair offer` shows six words. Device **A**: `nexus pair join "<six words>"`.
4. Both show **four words**. Compare them; answer `y` on both only if identical. If they differ, someone is interfering.
5. `nexus devices` / `nexus dashboard` show what is paired and whether it is online.

## 4. Everyday commands
| I want to… | Command |
|---|---|
| See my devices live, one key to act | `nexus dashboard` (↑↓, Enter connect, `t` terminal, `w` wake, `l` lock, Ctrl-K palette) |
| Watch and control a screen | `nexus remote view <device> [--audio]` |
| Hear its sound | `nexus remote view <device> --audio` (the device needs the `audio` grant; sharing on by default, `[audio] enabled = false` to stop) |
| Screenshot / type / click | `nexus remote screenshot|type|key|click <device> …` |
| Open a terminal | `nexus shell <device> [--name work]` (named = survives disconnect; detach `Ctrl-] .`, kill `Ctrl-] k`) |
| Copy files | `nexus ls <device>:`, `nexus cp ./a.zip <device>:/path/`, `nexus cp <device>:/path/b.zip .` (resumable, hash-verified) |
| Reach a service on it | `nexus forward <device> 5432` → `localhost:5432` |
| Use plain `ssh` through the tunnel | `~/.ssh/config`: `Host box` / `ProxyCommand nexus proxy box` then `ssh box` (the target's sshd listens on its own loopback) |
| Lock / sleep / restart / shut down | `nexus power <device> lock|sleep|restart|shutdown` |
| Wake a sleeping PC | `nexus wake <device> [--via <always-on device>]` |
| Pre-authorise a device (no prompt) | `nexus trust unattended <device>` |
| Answer "may this device use me?" prompts | `nexus consent` (or the desktop app) |
| Read what happened here | `nexus audit tail` / `nexus audit verify` |
| See who is connected to this machine | `nexus sessions` (the dashboard and desktop app show a red banner) |
| Cut them off | `nexus disconnect [device]`; `D` in the dashboard; **Ctrl+Alt+Shift+L** in the desktop app = lockdown + disconnect everyone |
| Emergency stop | `nexus lockdown on` |
| Check the setup | `nexus doctor` |

If the link drops (Wi-Fi switch, sleep, a restart of the controller's agent) the viewer keeps its last picture, says "reconnecting" in the title bar and comes back by itself; an attended target does not ask again if the device was approved moments before (`[security] resume_window_secs`, default 120, 0 = always ask).

Press the viewer's local hotkeys (`Ctrl+Alt+Shift+Q` quit, `F` fullscreen, `P` pixel-perfect) to leave or resize it.

## 5. Special machines
* **Headless Linux server (no monitor).** Install `xvfb`. The agent creates its own virtual screen automatically
  (`[display] virtual = "auto"`), so `nexus remote view server` shows a desktop. Put a window manager or terminal on it with
  `[display] session = ["openbox-session"]`. Without Xvfb the server still offers terminal, files and ports.
  The systemd unit is in `installers/linux/systemd/` (hardened; a root shell for administrators is an explicit, documented
  drop-in with a warning). Talk to a service agent with `sudo nexus --system …` (`pair offer`, `trust unattended`, `status`, `audit tail`);
  the per-user agent needs no flag.
* **An always-on helper that wakes your PCs.** On a Raspberry Pi / home server: `nexus-agent --wake-helper` (or the
  `nexus-wake.service` unit). Pair it with your devices and grant `wake`. It offers nothing else.
* **Windows PC (service).** [WINDOWS.md](WINDOWS.md): lock-screen and UAC control need the SYSTEM service (**untested**).
* **macOS.** Grant *Screen Recording*, *Accessibility* and *Input Monitoring* (and *System Audio Recording* for sound) in
  System Settings → Privacy & Security, then restart the agent. **Untested.**

## 6. Safety features
* Nothing connects to a device that did not pair with you; an unpaired device cannot even see that yours exists.
* By default initial pairing pre-authorises future connections, so an online PC needs only **Connect** on the controller.
  `nexus trust attended <device>` opts a paired device into approval for every session; `nexus trust unattended <device>` pre-authorises
  one device. Passkey/TOTP verification is optional: enable `security.unattended_requires_stepup` and enroll a factor with
  `nexus auth enroll-passkey` or `nexus auth enroll-totp`; the native viewer then prompts automatically. See [STEP_UP.md](STEP_UP.md). Three refusals in a row lock a device out of prompts for a while.
* `nexus unpair <name>` ends the relationship immediately and closes live sessions. `nexus lockdown on` refuses everything,
  survives restarts.
* Everything a controller does (screen, input, files with paths, terminals, forwards, power) is written to a signed,
  hash-chained log: `nexus audit verify` detects edits, deletions, reordering and a cut-off tail.
* LAN discovery announces only a short **rotating tag** that your paired devices can recognise (nobody else learns your
  device exists or what it is called). Turn it off with `[net] lan_discovery = false`.

## 7. Configuration
`config.toml` in the config directory (next to the identity and trust files; `--home DIR` puts everything under `DIR`); every key also works as `NEXUS_SECTION__KEY=value`.
```toml
[net]       signal_url = "wss://signal.example.com/v1/ws"; udp_port = 7842; lan_discovery = true
[security]  consent = "unattended"; unattended_requires_stepup = false; consent_timeout_secs = 30; resume_window_secs = 120
[files]     shared = [{ path = "/srv/share", writable = true }]
[clipboard] send = true; receive = true; images = true
[audio]     enabled = true; microphone = false; bitrate_kbps = 96   # optional: device = "monitor"
[display]   virtual = "auto"; size = "1920x1080"; session = []
[terminal]  enabled = true; shells = []
[portfwd]   enabled = true; loopback = true
[power]     accept_requests = true; wake_relay = false
```

## 8. Troubleshooting
| Symptom | Likely cause / fix |
|---|---|
| `Cannot reach the linktunnel agent` | the agent is not running, or runs under a different `--home` |
| Peer shows `offline` | its agent is not connected to the same signaling server (and not on your LAN) |
| `ping` times out, both online | UDP blocked / strict NAT: run a relay ([SELF_HOSTING.md](SELF_HOSTING.md)) and check firewalls |
| Pairing: server not connected | pairing needs the signaling server; `nexus status` |
| Words differ between screens | **do not pair**: interference or the wrong device |
| `remote view` black / "no display" | the target has no screen: install `xvfb` (Linux) or use `nexus shell` |
| No sound | the device needs the `audio` grant (re-pair: grants are fixed at pairing); on Linux the *monitor* source of the output must exist (PipeWire/PulseAudio) and `PULSE_SOURCE=<sink>.monitor` may be needed; macOS needs the *System Audio Recording* permission and 14.6+ |
| `terminal` refused on a Windows service | by design (`--no-terminal`, else it would be a SYSTEM shell); see WINDOWS.md |
| Attended prompt never appears on a headless box | nobody is connected to answer: use `nexus trust unattended` for that device, or run `nexus consent` |

## Native viewer additions

The viewer uses native GPU presentation where available. `NEXUS_SOFTWARE_VIEWER=1` forces the
software fallback. Ctrl+Alt+Shift+B toggles the target's Windows normal-desktop privacy shield
and local-input block (needs the `privacy` pairing grant). Physical Esc on the target restores
its display; disconnect also restores it. Unsupported platforms report refusal.
See [ACCELERATION.md](ACCELERATION.md) for optional vendor encoder builds and their current limits.
