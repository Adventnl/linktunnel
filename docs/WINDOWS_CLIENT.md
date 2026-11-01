# Windows controller

Extract the **entire** `linktunnel-windows-x64.zip` and double-click **LinkTunnel.cmd**.
Keep the three EXEs together: the dashboard launches the native viewer from `nexus.exe`.
Windows 10/11 x64 and Microsoft Edge WebView2 Runtime are required.

On a PC with the existing running Link Tunnel host, the launcher requests administrator
access and opens its dashboard with `--system`. It uses the host's existing identity,
pairings and signaling configuration. The same host can receive and initiate connections.
It does not need a second agent or a second installation of the Windows service.
If that service is stopped, start it with `Start-Service NexusAgent` in Administrator PowerShell first.

On a new PC, the launcher asks for the signaling URL used by your other devices, starts
a per-user controller agent, and opens the dashboard. Its settings are saved under
`%LOCALAPPDATA%\linktunnel\client-settings.json`; start LinkTunnel.cmd again after reboot.
The controller agent keeps running after the dashboard closes; `nexus.exe stop` stops it.
For a command-line first launch:

```powershell
.\launch-client.ps1 -SignalUrl wss://signal.example.com/v1/ws
# For your existing trusted Tailscale network only:
.\launch-client.ps1 -SignalUrl ws://100.114.106.93:8443/v1/ws -AllowInsecureSignal
```

Once the Mac or Linux machine is online, select **Connect** for its screen or **Terminal**
for its shell. If already paired with this Windows host, no new pairing is needed.
Otherwise choose Pair, enter the other device's offer, and compare the four verification
words on both machines. A Mac must be logged into its desktop with Screen Recording,
Accessibility and Input Monitoring permissions for screen/input control.

For Ubuntu Server, follow [the headless setup](UBUNTU_SERVER.md). A terminal-only server
offers **Terminal**; install the Xfce profile to use **Connect** with no physical monitor.

## Build the ZIP

From this repository on Windows with the existing Rust/Visual Studio prerequisites:

```powershell
.\installers\windows\build-client.ps1
# If the release binaries and desktop app have already been built:
.\installers\windows\build-client.ps1 -SkipBuild
```

Output: `dist/linktunnel-windows-x64.zip`, with SHA-256 hashes in the extracted folder.
This is a portable build, not a signed installer. If WebView2 is absent, install Microsoft's
[Evergreen WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/).
