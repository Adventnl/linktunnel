# Native Windows cellular calls

Link Tunnel now has its own Windows Bluetooth call bridge and dialer. It does
not launch, install or depend on Microsoft Phone Link. The phone supplies its
SIM and mobile service; Windows transports two-way HFP audio using the PC's
default **communications** microphone and output. No call is recorded.

This is source implemented and software tested. Actual Redmi call audio is
**not hardware verified**. The user deferred Bluetooth pairing/live testing.
Do not interpret a successful build, transport connection, or browser test as
proof that a call's audio has moved to the PC.

## Setup

1. Use the call-enabled Link Tunnel package below. The ordinary unpackaged
   NSIS/portable executable has no Windows identity/call capabilities.
2. Pair the Redmi with the PC in **Windows Bluetooth settings**, and allow
   calling/audio permissions on the phone. This is separate from adb's USB or
   Wireless debugging pairing. The phone must remain powered on and nearby.
3. Open **Phone → Cellular calls on this computer**. Choose the exact Bluetooth
   phone, then **Connect call audio**. No phone is automatically connected.
4. Choose its SIM/line, enter the number and select **Call using PC audio**.
   Leave the mirroring panel's media-audio option off. Select the desired
   Windows default communications microphone/output in **Audio settings**.
5. On Windows versions exposing per-call control, **Take audio on PC** transfers
   a call started in the mirrored phone dialer. **Answer on PC**, mute and End
   operate on the selected phone's live calls only.

**Call audio: this computer** appears only when Windows reports
`PhoneCall.AudioDevice == LocalDevice` for that particular live call. Transport
`CanRouteToLocalDevice` merely means a route is available. It is not a live
audio assertion. Make a non-emergency test call and verify both directions.

Closing the panel leaves the call bridge connected. **Disconnect call audio**
and app exit release this app's Windows transport registration; they do not
invoke Hang Up. Disconnecting screen mirroring is separate. The phone screen
may be dark, but this does not shut the phone down or bypass App Lock. Ringing,
screen wake and phone routing behavior still require Redmi acceptance testing.

## Windows version and permissions

The bridge uses Microsoft's documented `PhoneLineTransportDevice` APIs:

- `RequestAccessAsync`, `RegisterApp`, **checked** `IsRegistered`, and
  `ConnectAsync` establish the HFP transport.
- A `PhoneLineWatcher` discovers the selected transport's lines. Every action
  re-resolves lines and calls belonging to that transport; arbitrary call IDs
  are never passed to `PhoneCall.GetFromId`.
- Newer Windows versions expose `PhoneLine.GetAllActivePhoneCallsAsync` and
  `PhoneCall.ChangeAudioDeviceAsync(LocalDevice)`. Availability is checked at
  runtime, rather than inferred from the Windows marketing version.
- On this Windows 10 build 19045, the transport API exists but the newer
  per-call API does not. The legacy dialer requests local speakerphone with
  `PhoneDialOptions`; this is only a hint Windows may ignore, so its button is
  labeled **Request call (audio unverified)**. Audio transfer/route verification, answer and End are
  unavailable in this mode; the interface says so and leaves those actions to
  the mirrored phone. **Legacy two-way audio is not verified.**

A real package identity with `phoneLineTransportManagement`, `phoneCall` and
Bluetooth/microphone declarations is needed. The package is a full-trust Win32
app; it uses the existing installed desktop host when present. It does not
install another system service or enable a driver/registry workaround.

Some Windows builds may deny transport registration even after returning
Allowed for access. The bridge checks registration and reports the failure
without dialing or declaring audio connected. It does not use private tokens,
impersonate another app or change Windows security policy.

References: [transport API](https://learn.microsoft.com/en-us/uwp/api/windows.applicationmodel.calls.phonelinetransportdevice),
[required capability](https://learn.microsoft.com/en-us/uwp/api/windows.applicationmodel.calls.phonelinetransportdevice.requestaccessasync),
[call audio devices](https://learn.microsoft.com/en-us/uwp/api/windows.applicationmodel.calls.phonecallaudiodevice),
[restricted capability declarations](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/app-capability-declarations).

## Building and installing

From the repository root on Windows, after the ordinary desktop host/viewer
bundle has been built:

```powershell
./installers/windows/build-calls-msix.ps1
```

This builds the current frontend and call-enabled app into a separate binary,
preserving a running desktop executable. It stages the bundled Android tools,
native viewers and the existing app icon, then runs SDK MakeAppx validation.
It outputs `dist/LinkTunnel-Calls-0.1.0.0-x64-unsigned.msix` and records its
development directory in `dist/LinkTunnel-Calls-development-directory.txt`.

For local development **when Developer Mode is already enabled**, quit Link
Tunnel from its tray and register that build:

```powershell
$callBuild = (Get-Content ./dist/LinkTunnel-Calls-development-directory.txt -Raw).Trim()
./installers/windows/install-calls.ps1 -DevelopmentDirectory $callBuild
```

Open the newly registered **Link Tunnel** entry from Start. Do not remove its
development directory while it is registered. The script neither enables
Developer Mode nor trusts a certificate. To remove this development package:

```powershell
Get-AppxPackage -Name LinkTunnel.Desktop.Calls | Remove-AppxPackage
```

For distribution, provide an existing trusted code-signing certificate from
CurrentUser\My; the publisher must exactly match its subject:

```powershell
./installers/windows/build-calls-msix.ps1 -Publisher 'CN=Your Publisher' -SigningCertificateThumbprint YOUR_CERTIFICATE_THUMBPRINT
./installers/windows/install-calls.ps1 -PackagePath ./dist/LinkTunnel-Calls-0.1.0.0-x64.msix
```

No certificate or private key is generated, imported or stored by these scripts.
An unsigned MSIX is a build/review artifact, not an installable release.

## Verification and remaining acceptance

- `cargo test -p nexus-phone`: native read-only enumeration and rejected-action
  checks, phone-number validation, existing subprocess/lifecycle tests.
- Strict `cargo clippy -p nexus-phone --all-targets -- -D warnings`.
- Tauri native check and a release app build; SDK MakeAppx schema/payload check.
- Frontend unit tests, typecheck/build and real React/Tauri-bridge browser checks
  with **simulated native calling replies**. They cover exact Bluetooth/line
  targeting, no automatic dialing, unconfirmed versus confirmed call audio,
  transfer, incoming answer, mute, End, persistent operation errors, stale-state
  handling, release, panel-close persistence, package/hardware/version limits,
  compact layouts and Chinese/dark mode.
- Read-only `cargo run -p nexus-phone --example call-diagnostics` distinguishes
  OS API support, visible Bluetooth adapter, package identity, per-call API
  support, transport count and communications audio endpoints. It never
  registers, connects, dials or records. In this session Windows exposed no
  Bluetooth adapter or phone; the user says Bluetooth hardware is present, so
  driver/service/pairing visibility still needs checking.

Still required: launch with real package identity; pair the Redmi; verify
registration, the correct SIM, outbound and inbound calls, caller hearing only
the selected PC mic, caller audio on the PC, phone speaker/mic routing, screen
dark behavior, disconnect/reconnect and app-exit cleanup. Call-control support
on newer Windows and Windows 10 legacy routing require separate hardware tests.
