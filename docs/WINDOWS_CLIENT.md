# Windows controller

Run **linktunnel_0.1.0_x64-setup.exe** once, then open **Link Tunnel** from the Desktop or Start menu.
Setup requests administrator permission to install the background service. Everyday app launches
run as your desktop account, without CMD/PowerShell windows or another elevation prompt.
Windows 10/11 x64 and Microsoft Edge WebView2 Runtime are required.

The setup reuses an existing Link Tunnel host's identity,
pairings and signaling configuration. The same host can receive and initiate connections.
It installs the dashboard, native viewer and supervised service together. Reopening the shortcut
brings the existing app forward. Native viewer subprocesses create their normal GUI without a console.
The registered installer can be removed from Windows **Installed apps**; device state is retained.

The installer enrolls the Windows account that runs setup for local host controls. Only its SID,
SYSTEM and Administrators can use that local pipe; the owner can read runtime IPC files, while
device keys, trust and configuration remain protected. Remote pipe clients are rejected.
Do not grant all users access to `C:\ProgramData\Nexus` to avoid elevation.

The setup EXE currently upgrades a configured host. To provision a new PC with no signaling
configuration, run the bundle's installation script once from Administrator PowerShell:

```powershell
.\install-desktop.ps1 -SignalUrl wss://signal.example.com/v1/ws
# For your existing trusted Tailscale network only:
.\install-desktop.ps1 -SignalUrl ws://YOUR-SERVER:8443/v1/ws -AllowInsecureSignal
```

Once the Mac or Linux machine is online, select **Connect** for its screen or **Terminal**
for its shell. If already paired with this Windows host, no new pairing is needed.
Otherwise choose Pair, enter the other device's offer, and compare the four verification
words on both machines. A Mac must be logged into its desktop with Screen Recording,
Accessibility and Input Monitoring permissions for screen/input control.

For Ubuntu Server, follow [the headless setup](UBUNTU_SERVER.md). A terminal-only server
offers **Terminal**; the GNOME profile provides **Connect** with no physical monitor.

## Build the Windows setup

From this repository on Windows with the existing Rust/Visual Studio prerequisites:

```powershell
.\installers\windows\build-desktop.ps1
# Reuse the existing release host/viewer binaries:
.\installers\windows\build-desktop.ps1 -SkipBuild
```

Output: `dist/linktunnel_0.1.0_x64-setup.exe` (NSIS). Distribution signing requires your own
code-signing certificate. The legacy `build-client.ps1` portable ZIP and command launcher remain
available for development; installed shortcuts must target `nexus-app.exe`, not that CMD file.
If WebView2 is absent, install Microsoft's
[Evergreen WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/).
