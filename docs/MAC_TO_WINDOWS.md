# MacBook to Windows PC

Use the same repository revision on both machines. Start on the home network, then test from an external network. A successful local screenshot does not prove UAC, login, or outside-home access.

## Windows host

Build with `installers/windows/build-host.ps1`. It needs Rust, Visual Studio C++ Build Tools (including the Windows SDK/CMake), Strawberry Perl for vendored OpenSSL, and NASM 2.16.03 for optimized software H.264. NASM 3.02 crashed while producing the codec's Windows debug information during native verification. Use `scripts/check-windows.ps1` to run checks with the same native build environment. The optional desktop dashboard is built separately with `npm ci`, `npm run build`, and `cargo build --release --locked --manifest-path apps/nexus-app/src-tauri/Cargo.toml`.

In Administrator PowerShell, from `target/windows-host`:

```powershell
.\install-host.ps1 -LanAddress 192.168.1.32 -KeepAwakeOnAc
& 'C:\Program Files\Nexus Remote\nexus.exe' --system doctor
& 'C:\Program Files\Nexus Remote\nexus.exe' --system pair offer
```

Replace the IP with the PC's actual private LAN address. Reserve this address in your router before relying on it after a reboot. The installer puts binaries in Program Files, restricts state to SYSTEM/Administrators, starts the supervised SYSTEM host automatically, and runs a persistent SQLite-backed LAN signaling server at boot. Firewall rules permit the local subnet only, including when Windows classifies the home network as Public. Windows Home is supported; Windows RDP hosting is not required.

`-KeepAwakeOnAc` disables timed sleep/hibernation while plugged in, so the host remains reachable. The previous settings are saved in `C:\ProgramData\Nexus\power-settings-before.txt`; omit the switch to retain the current power settings.

The LAN trial uses plain WebSocket signaling, so connection metadata is visible on that trusted network. Pairing and remote session content retain their cryptographic protection. Do not forward this LAN server's port on the router.

For an existing public TLS signaling server, use `-SignalUrl wss://your-host/v1/ws` instead. Existing configuration and identity are preserved; changing servers requires an explicit configuration edit.

The Windows dashboard can manage the host when launched **as Administrator** with `nexus-app.exe --system`. The ordinary dashboard manages a per-user agent. The CLI uses `--system` for every host-management command.

## MacBook controller

On the Mac, open this repository and build the native Mac binaries:

```sh
cargo build --release --locked -p nexus-agent -p nexus-cli
./target/release/nexus-agent --name MacBook --no-screen \
  --signal-url ws://192.168.1.32:8443/v1/ws --allow-insecure-signal
```

Keep that agent terminal open for the first test. `--no-screen` makes the laptop a controller without sharing its screen; Screen Recording/Accessibility access is needed if you later want to control the Mac itself. In a second Mac terminal:

```sh
./target/release/nexus status
./target/release/nexus pair join "the six words shown on Windows"
```

Compare the four verification words on **both** machines and confirm only if they match. On Windows, keep the offer terminal open while joining. The default offer grants the product's remote features, but the SYSTEM host still disables remote terminals and shares no folders by default.

For the first attended connection, with someone at the unlocked PC:

```sh
./target/release/nexus devices
./target/release/nexus remote view "the PC name shown by nexus devices" --audio
```

Approve the visible request on Windows. Verify live updates, mouse, typing, scrolling, modifier keys, monitor selection, clipboard and audio. Use `Ctrl+Alt+Shift+Q` to quit the viewer.

## Unattended access, UAC and login

On Windows, in Administrator PowerShell:

```powershell
nexus --system auth enroll-passkey
# Or enroll an authenticator app instead:
nexus --system auth enroll-totp
nexus --system auth status
nexus --system trust unattended MacBook
```

Use a synced passkey available on the Mac or a portable security key. For TOTP, scan the enrollment URI privately and prove possession with the current code. Do not send enrollment secrets through chat. The viewer opens the verification browser automatically. Leave `unattended_requires_stepup = true` enabled.

With your Mac viewer connected, test an elevated app/UAC prompt, then lock the PC and sign in using your own Windows credentials from the Mac. `Ctrl+Alt+Shift+D` sends Ctrl+Alt+Del. Keep local access available during this first test. Check `docs/FULL_CONTROL_QA.md` for the full device acceptance checklist.

## Outside the home network

The private `192.168.x.x` signaling URL cannot be used from outside home. Complete LAN acceptance first, then configure a publicly reachable TLS signaling server and an HTTPS relay using `docs/SELF_HOSTING.md`, or place both devices on a private VPN that carries their signaling/direct traffic. That deployment needs the chosen server/domain or VPN connection details; the local installer does not create public hosting.

Configure both agents for the reachable server, retain their existing identities and pairing stores, then test from a phone hotspot or other external connection. Verify direct and relay-only paths, reconnect, UAC/login, audio and latency. Do not consider WAN access complete until that separate-network test passes.

## Recovery and diagnosis

In Administrator PowerShell: `nexus --system status`, `nexus --system doctor`, `Get-Service NexusAgent`, and `Get-ScheduledTaskInfo NexusSignal`. Logs live under `C:\ProgramData\Nexus\logs` and `C:\ProgramData\Nexus\signal\logs`. Two `nexus-agent` processes should exist: the service in session 0 and its child in the active desktop session.

Stop remote access with `nexus --system lockdown on` or `Stop-Service NexusAgent`. The physical panic shortcut is `Ctrl+Alt+Shift+Esc`. Re-enable access deliberately with `nexus --system lockdown off`. Keep the PC awake during initial tests; wake from sleep is a separate hardware/network check.
