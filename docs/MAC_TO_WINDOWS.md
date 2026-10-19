# MacBook to Windows PC

Use the same repository revision on both machines. Start on the home network, then test from an external network. A successful local screenshot does not prove UAC, login, or outside-home access.

## This PC is installed (2026-10-15)

The host on `DESKTOP-H9VGBNO` is installed in `C:\Program Files\Nexus Remote` and running as
an automatic SYSTEM service. After Tailscale configuration, its signaling URL is
`ws://100.114.106.93:8443/v1/ws`. Connect the Mac to the same Tailscale account/network.
Diagnostics pass and the real-desktop two-agent smoke test passes. Keep the PC plugged in;
the installer disabled timed sleep and hibernation on AC power. Reserve `192.168.1.32` in the
router before relying on the LAN address as a fallback.

The operator reports that fresh administrator checks passed with the Tailscale address:
Nexus connects, startup is automatic, VPN firewall rules are enabled, and plugged-in sleep
is disabled. Mac pairing, factor enrollment, UAC/login and a phone-hotspot test remain
pending; these Windows checks do not verify outside-home control.

Continue with **MacBook controller** below; the Windows build/install commands do not need to
be repeated on this PC. Open **PowerShell as Administrator** on the PC when ready to pair:

```powershell
& 'C:\Program Files\Nexus Remote\nexus.exe' --system pair offer
```

The paired name to use in the Mac viewer is `DESKTOP-H9VGBNO`. No Mac pairing or factor
enrollment has happened yet. Updated personal hosts allow the paired Mac to reconnect without another Windows prompt.

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

On the Mac, install/open Tailscale and sign in using the same account as the PC. Enable
Tailscale's **Start at login** setting. Check that the Windows peer is online and that its
signaling port is reachable (the App Store app includes the CLI at the path below):

```sh
/Applications/Tailscale.app/Contents/MacOS/Tailscale status
/Applications/Tailscale.app/Contents/MacOS/Tailscale ping 100.114.106.93
nc -vz -w 5 100.114.106.93 8443
```

If Tailscale reports that it cannot set startup, add `/Applications/Tailscale.app` under
**System Settings → General → Login Items & Extensions → Open at Login** instead.

Open this repository and build the native Mac binaries:

```sh
cargo build --release --locked -p nexus-agent -p nexus-cli
./target/release/nexus-agent --name MacBook --no-screen \
  --signal-url ws://100.114.106.93:8443/v1/ws --allow-insecure-signal
```

Keep that agent terminal open for the first test. `--no-screen` makes the laptop a controller without sharing its screen; Screen Recording/Accessibility access is needed if you later want to control the Mac itself. In a second Mac terminal:

```sh
./target/release/nexus status
./target/release/nexus pair join "the six words shown on Windows"
```

Compare the four verification words on **both** machines and confirm only if they match. On Windows, keep the offer terminal open while joining. The default offer grants the product's remote features, but the SYSTEM host still disables remote terminals and shares no folders by default.

After pairing, open Nexus Remote on the Mac. An online PC shows **Connect**; pressing it opens the live desktop. New hosts pre-authorise paired devices, so nobody needs to approve on Windows again. **Wake** is Wake-on-LAN for a sleeping/offline PC, not part of connecting to an online PC; it requires compatible hardware and network configuration.

For a Windows host installed with the earlier approval/step-up defaults, run this migration once in Administrator PowerShell from the updated repository:

```powershell
./installers/windows/enable-personal-access.ps1 -Controller MacBook
```

This pre-authorises the paired Mac, saves a configuration backup, sets paired access as the default, disables the optional per-session factor prompt, and restarts the host. Existing explicit per-device approval choices remain available through `nexus --system trust attended <device>`. Future connections need only **Connect** on the Mac.

The dashboard waits for a decoded remote picture before saying it is connected. Startup errors appear on the device card, and a stalled startup times out so Connect can be retried.

## Optional extra verification, UAC and login

For a passkey or TOTP prompt on the controller for every connection, follow [step-up setup](STEP_UP.md) and explicitly enable `security.unattended_requires_stepup = true` on Windows. This is optional and is separate from initial pairing.

With your Mac viewer connected, test an elevated app/UAC prompt, then lock the PC and sign in using your own Windows credentials from the Mac. `Ctrl+Alt+Shift+D` sends Ctrl+Alt+Del. Keep local access available during this first test. Check `docs/FULL_CONTROL_QA.md` for the full device acceptance checklist.

## Outside the home network

The selected deployment uses Tailscale on both devices and the same
`ws://100.114.106.93:8443/v1/ws` signaling URL at home and away. Plain WebSocket signaling
travels inside the encrypted VPN; use this address only with Tailscale connected. Keep
router port forwarding disabled. The previous private `192.168.x.x` URL is only a LAN fallback.

For another Windows host, run `installers/windows/configure-tailscale.ps1` in Administrator
PowerShell after Tailscale sign-in. It preserves Nexus identity/pairings, enables Windows
Tailscale unattended mode, restricts VPN firewall rules to the Tailscale adapter/address,
and makes signaling startup wait for that adapter. This PC has already been configured.

A public TLS signaling server and HTTPS relay are an alternative deployment described in
`docs/SELF_HOSTING.md`; they are not required for the selected VPN trial.

Retain existing identities and pairing stores, then connect the Mac to a phone hotspot or
other external connection while leaving Tailscale connected. Repeat reachability and live
control checks, reconnect, UAC/login, audio and latency. Record whether Tailscale uses a
direct or DERP path separately from Nexus's transport classification. A Nexus relay-only
test requires a separately configured Nexus relay. Do not consider WAN access complete
until the separate-network live-control test passes.

## Readable desktop on weak Wi-Fi — controller update (2026-10-19)

The updated Mac agent requests native-resolution video and adapts frame rate to the available
bitrate. It prioritises readable text when Wi-Fi or a relayed VPN path slows down; motion can
be less smooth on very slow links. This uses the existing `SetQuality` protocol, so it works
with the installed Windows host without rebuilding that host. It applies to sessions opened
through the Mac agent, including the dashboard and `nexus remote view`.

Delayed older video streams no longer move the bandwidth estimator's delay baseline backwards.
The Mac also republishes changed transport addresses on its activity check (normally within
10 seconds), rather than waiting up to a minute after a network change.

For access from elsewhere in New Zealand, leave Tailscale connected on the Mac and the Windows
PC online and awake. The same VPN signaling address above works across networks; a public
router port is not needed. Tailscale can use its encrypted relay fallback when a network will
not allow a direct connection ([Tailscale connection types](https://tailscale.com/docs/reference/connection-types)). Direct/relay quality and phone-hotspot operation still need a
live test with the PC online.

On 2026-10-19 the Mac's Tailscale backend was running with no health warnings. Its network
check allowed UDP but reported destination-dependent NAT and a Sydney fallback relay. This
can make a direct connection harder; it does not establish which path a live session will use.
The PC was offline, so neither its current firewall/service state nor outside-home control
could be checked.

## Recovery and diagnosis

In Administrator PowerShell: `nexus --system status`, `nexus --system doctor`, `Get-Service NexusAgent`, and `Get-ScheduledTaskInfo NexusSignal`. Logs live under `C:\ProgramData\Nexus\logs` and `C:\ProgramData\Nexus\signal\logs`. Two `nexus-agent` processes should exist: the service in session 0 and its child in the active desktop session.

Stop remote access with `nexus --system lockdown on` or `Stop-Service NexusAgent`. The physical panic shortcut is `Ctrl+Alt+Shift+Esc`. Re-enable access deliberately with `nexus --system lockdown off`. Keep the PC awake during initial tests; wake from sleep is a separate hardware/network check.
