# Android phone control from Windows and Mac

The desktop app's **Phone** button opens a movable, resizable native scrcpy window
for the exact authorised phone selected in the panel. It works independently of
the desktop agent. This is separate from the Android Codex companion APK, which
controls Codex sessions on a computer.

## Capabilities

| Capability | Implementation / limits |
|---|---|
| Screen, touch, dragging, scrolling, keyboard, clipboard | Native scrcpy over USB or an adb network transport; no root or companion APK |
| Smooth control | H.264, 8 Mbps, 60 fps cap, maximum dimension of 1080 / 1600 / 2560 pixels; actual performance depends on hardware and network |
| Phone display off | Default, via `--turn-screen-off`; phone must remain powered on |
| Ordinary media audio | Optional, off by default, Android 11+; separate from cellular calls |
| Wireless pairing | Android 11+ pairing code; separate pairing and connection ports |
| Session lifecycle | First-frame startup check, bounded diagnostics/deadlines, one managed viewer, close detection, Stop and cancellation on app exit |
| Windows cellular calls | Opens external Windows Phone Link; configure Bluetooth Calls and transfer there, then verify audio |
| Android cellular-call audio on Mac | Not implemented; use a headset connected directly to the phone |
| Fully powered-off phone | Cannot be controlled or woken by this implementation |
| Internet / Nexus relay | Not implemented for phones; needs USB or a reachable wireless-debugging endpoint |

PINs, first unlock after boot, protected content, Android lock rules and HyperOS
permissions still apply. Not every application can be mirrored or every gesture
reproduced. This does not bypass an Android lock.

## Desktop prerequisites

Install **scrcpy 3.3+** and **Android SDK Platform Tools** on each computer.
They are external dependencies, not bundled in the Nexus installer.

Windows ([official scrcpy installation guide](https://github.com/Genymobile/scrcpy/blob/master/doc/windows.md)):

```powershell
winget install --id Genymobile.scrcpy --exact
winget install --id Google.PlatformTools --exact
```

Mac, using Homebrew:

```sh
brew install scrcpy
brew install --cask android-platform-tools
```

Restart Nexus to inherit the new PATH. The panel reports tool versions or missing
prerequisites. For a portable installation, set `NEXUS_SCRCPY` and `NEXUS_ADB` to
absolute executable paths in Nexus's launch environment. An absolute scrcpy path
uses its adjacent adb if available. The same adb is passed to scrcpy, keeping
discovery and control on the same server and credentials. Mac GUI apps may need
these overrides when Homebrew is absent from their PATH. Use native executables,
not `.bat` wrappers.

## Redmi Note 14 Pro: USB setup

1. In Settings, enable Developer options, usually by tapping the OS / HyperOS
   version repeatedly under About phone; wording varies.
2. Under Additional settings → Developer options, enable **USB debugging**.
3. Connect a data-capable USB cable. Unlock the phone and accept this computer's
   debugging prompt. Persist authorisation only on a computer you trust.
4. In Nexus → **Phone**, select **Refresh devices and tools**, choose the Redmi,
   then **Open phone**. Offline/unauthorised devices are listed but cannot open.

If mirroring works but clicking does not, Xiaomi may require **USB debugging
(Security settings)**. Enable it on the phone and reboot if requested. This varies
by HyperOS build; Nexus does not grant permissions silently. See the
[scrcpy prerequisites](https://github.com/Genymobile/scrcpy#prerequisites).

**Keep phone display off** keeps the physical screen dark, with the phone still
powered on. Click/drag to swipe, use the scroll wheel, right-click for Back and
middle-click for Home. Alt / Option + F toggles fullscreen; Alt / Option + Shift
+ O turns the phone display on. Keyboard and clipboard behaviour follows
[scrcpy input support](https://github.com/Genymobile/scrcpy/blob/master/doc/input.md).

Close the phone window or select **Stop phone control** to end control. Closing
only the panel keeps the native window running. Quitting Nexus stops the viewer,
including a connection still starting.

## Wireless debugging

1. Use the same reachable Wi-Fi. Enable **Developer options → Wireless debugging**.
2. On the phone, select **Pair device with pairing code**. In Nexus, expand
   Wireless pairing. Enter the IP/port from that dialog and the six-digit code.
3. Select **Pair phone**. Nexus clears the code on submission, passes it through
   stdin rather than process arguments, and does not log or save it. Android/adb
   own the saved pairing credentials.
4. Return to the phone's main Wireless debugging screen. Its connection port is
   usually different. Enter that IP/port under **Connection**, select **Connect
   wirelessly**, then choose the resulting device and open it.

Use numeric `IP:port` (IPv6: `[address]:port`). Ports can change after reboot or
Wi-Fi changes. Network isolation or Android disabling wireless debugging can
prevent reconnection. This implementation does not enable legacy `adb tcpip 5555`,
open router ports or add a Nexus internet relay. See the
[Android wireless adb guide](https://developer.android.com/tools/adb#connect-to-a-device-over-wi-fi).

**Disconnect address** disconnects that transport without revoking credentials or
killing the shared adb server. Revoke access on the phone via Wireless debugging
→ Paired devices → Forget, or revoke USB debugging authorisations. Disable
debugging when no longer needed. Do not expose adb ports publicly.

## Cellular calls and the computer microphone

Mirroring and media audio do **not** implement two-way cellular-call audio.
Android restricts voice-call capture; scrcpy does not provide general computer-mic
injection into a cellular call. See [Android audio-input restrictions](https://developer.android.com/media/platform/sharing-audio-input)
and [scrcpy audio](https://github.com/Genymobile/scrcpy/blob/master/doc/audio.md).

On **Windows**, select **Open Windows Phone Link**, set up **Calls**, and approve
Bluetooth pairing/call permissions. During a call use Phone Link's transfer
control to move it to the PC. The phone still supplies cellular service. Select
PC audio devices and test both directions. Nexus cannot inspect the audio route,
silence the phone or confirm transfer. Leave Nexus media audio off with this route.

Phone Link requires Bluetooth proximity and remains a separate application.
Microsoft documents headset restrictions, including Bluetooth-headset forwarding;
arbitrary PC headphones are not guaranteed. Consult [Calls setup](https://support.microsoft.com/en-us/windows/apps/phonelink/setting-up-calls-in-the-phone-link)
and [Calls troubleshooting](https://support.microsoft.com/en-us/windows/apps/phonelink/troubleshooting-calls-in-the-phone-link).
The launcher uses Microsoft's documented [Phone Link URI](https://learn.microsoft.com/en-us/windows/apps/develop/launch/launch-settings#phone).

On **Mac**, Android cellular-call audio handoff is not implemented. Use a headset
directly connected to the phone while controlling its screen. Moving audio to the
Mac needs a separate supported telephony/Bluetooth implementation and device tests.

## Authorisation

This uses Android's USB/wireless authorisation, **not Nexus desktop pairing**.
Android owns the adb keys. Nexus desktop grants, session consent, lockdown,
emergency disconnect and audit do not govern phone sessions. The panel states
this and provides a separate Stop action. No adb shell commands, lock bypass,
automatic port exposure, call recording or microphone capture are added.

## Verification

Verified on Windows during implementation:

- `cargo test -p nexus-phone`: parser properties, input/options/version validation,
  real subprocess pipes, 3.x/4.x frame detection, failures, output caps, deadlines,
  cancellation and session cleanup.
- `cargo clippy -p nexus-phone --all-targets -- -D warnings`.
- `cargo check --manifest-path apps/nexus-app/src-tauri/Cargo.toml`.
- Native desktop library tests: 9 passed on Windows; strict native clippy passed.
- Desktop `npm test` and `npm run build`, including locale key/placeholder parity.
- `npm run test:phone`: temporary Vite server, real React UI and Tauri bridge
  in Chromium with **simulated native replies**, covering Windows/Mac capability
  differences, missing tools, offline agent, exact device/options, pairing ports,
  code clearing, failures, stop and closed-window status.

**Not hardware-verified:** Redmi USB/wireless input, display-off behaviour, media
routing, measured latency/fps, Mac native execution, Phone Link call transfer,
computer microphone or headphone routing. No phone was connected. Browser tests
do not prove phone or Bluetooth compatibility.

Acceptance with your phone: USB first, then Wi-Fi; drag/scroll/type/paste; turn the
physical display off; lock/unlock with your normal PIN; close/reopen; disconnect
cable/Wi-Fi; verify Stop and app quit. On Windows, make a test call and confirm the
other person hears the PC microphone, their voice uses your selected PC output,
and the phone is not also outputting/capturing call audio.
