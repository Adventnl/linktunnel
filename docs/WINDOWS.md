# Windows: service, secure desktop, UAC and the lock screen

> **Native Windows host now installed and running.** The locked release build and same-PC
> encrypted physical-desktop smoke test pass. The boot-time SYSTEM service and session child
> are running, and installed host diagnostics pass. Mac-to-PC UAC/login, physical privacy,
> reboot recovery and outside-home access remain unverified. See the latest [STATUS.md](STATUS.md) entry.

The Windows control source now selects the native injector, recovers capture across secure-desktop
and display transitions, follows an active RDP session when the physical console is disconnected,
and routes Ctrl+Alt+Del through the SCM service. Touch/pen, embedded DPI awareness, monitor changes,
a target indicator and a physical panic shortcut are implemented. [ADR 0024](adr/0024-windows-control-completion.md)
records the details and implementation limits.

## The problem
| Obstacle | Why |
|---|---|
| Services live in **session 0**, which has no desktop | since Vista, session-0 isolation: nothing a service draws or captures is what the user sees |
| A user-mode process cannot see the **secure desktop** | the UAC consent prompt, the lock screen and the sign-in screen run on `Winsta0\Winlogon`, which only SYSTEM may open |
| Input and capture follow the **calling thread's desktop** | a process that captured `Default` goes blind (and its `SendInput` goes nowhere) the instant UAC appears |
| The console session changes | fast user switching, logoff/logon, an RDP user taking the console |

## The approach
```
 SCM ─▶ nexus-agent.exe --windows-service        SYSTEM, session 0   (crate nexus-winservice)
            │   WTS session notifications → Supervisor (portable state machine)
            ▼
        nexus-agent.exe --system --no-terminal   SYSTEM token, retargeted to the CONSOLE session
            │   capture thread + input thread call ThreadDesktop::sync()   (crate nexus-desktop)
            ▼
        OpenInputDesktop → SetThreadDesktop → rebuild the DXGI duplication  (Default ⇄ Winlogon)
```
1. **Launching into the console session** (`launcher.rs`): duplicate the service's own SYSTEM token
   (`OpenProcessToken` → `DuplicateTokenEx(TokenPrimary)`), retarget the copy with
   `SetTokenInformation(TokenSessionId = WTSGetActiveConsoleSessionId())`, build an environment block,
   `CreateProcessAsUser` on `winsta0\default`. Needs `SeTcbPrivilege`, which LocalSystem services hold.
2. **Following the desktop** (`nexus-desktop`): before each capture attempt and each input event the thread asks `OpenInputDesktop` for the desktop that has the user's attention, and if its
   name differs from the one it is attached to it `SetThreadDesktop`s to it. Capture then rebuilds the
   Desktop Duplication (which is bound to a desktop) and emits a full frame. When the thread is **not** SYSTEM,
   attaching to `Winlogon` fails with *access denied*; capture waits and input refuses the operation
   until an accessible desktop returns.
3. **Replacing the child** when the console session changes (`supervisor.rs`): kill, relaunch in the new session;
   crashes restart with 1 s → 30 s exponential back-off, reset after a minute of health. Lock/unlock of the same
   session does *not* restart anything: the desktop switch is handled in-process.
4. **Asking the person at the PC** (`prompt.rs`, `wts_prompter.rs`): a SYSTEM agent's IPC is admin-only, so the
   attended-access prompt is a `WTSSendMessage` Yes/No box on the console session with a timeout. It cannot be
   shown on the secure desktop, so a request that arrives **while the PC is locked is refused after the timeout**.
   Use an *unattended* (pre-authorised) device for the lock screen: that is what "I type my password remotely at the
   login screen" requires anyway.

## Privileges, exactly
| Thing | Needs | Why |
|---|---|---|
| Install the service | Administrator (MSI) | writes `HKLM\...\Services` |
| `launcher.rs` | `SeTcbPrivilege` (LocalSystem has it) | set `TokenSessionId` |
| Attach to `Winlogon` | SYSTEM in that session | `OpenInputDesktop` access check |
| State folder | SYSTEM + Administrators only | it holds the device identity key; the service replaces the state tree DACL on every start and refuses reparse points; the MSI sets the initial directory ACL |
| CLI talking to the agent | Administrator (elevated terminal) | the SYSTEM agent's IPC token file lives in the locked-down folder |

**Code signing is not required** for any of the above to work: reaching `Winlogon` is a property of running as SYSTEM,
not of a signature. (A *UIAccess* process, which we do not use, would need a signed binary in a trusted location.) Signing
is needed only to avoid SmartScreen/Defender friction when distributing; `installers/windows/build-msi.ps1` signs when
given a certificate thumbprint.

## Safety decisions
* **No terminal in the SYSTEM service by default.** A remote shell there would be a SYSTEM shell. The service passes
  `--no-terminal`; the operator can opt in with `--allow-system-terminal` and must understand that anyone holding the
  `terminal` grant then has administrator-grade access.
* **Files** are limited to the shared folders configured, exactly as on every other platform.
* **Power actions, clipboard, input** follow the same grants and consent rules as elsewhere.
* The session child has its own visible normal-desktop badge and physical **Ctrl+Alt+Shift+Esc**
  panic hook, independent of the dashboard. Clicking the badge disconnects current sessions and
  allows paired devices to reconnect. The panic shortcut disconnects sessions and enables lockdown.
  Re-enable access with the existing local lockdown command after using panic.

## Known limits (honest list)
* **Protected content** (DRM video) is black in Desktop Duplication, by design of Windows.
* **RDP**: the supervisor selects an existing active RDP session if the console is not active.
  It runs one child at a time, uses GDI fallback where duplication is unavailable, and reconnects on
  session replacement. It creates no extra sessions and does not change Windows licensing limits.
* **Multiple monitors with mixed DPI**: MSVC agent and controller binaries embed PerMonitorV2
  awareness. Monitor DPI is reported separately from physical pointer/cursor coordinates; layout
  changes update the viewer, injector and cursor sampler. Portrait displays rotate captured pixels.
* **HDR**: the duplication returns the tone-mapped SDR image Windows composes; no HDR passthrough.
* **Windows Server Core** has no desktop: the agent offers terminal/files/ports only (the virtual-display path is Linux-only).
* **Modern Standby**: presence reports sleep via power events; Wake-on-LAN from Modern Standby depends on the NIC and
  OEM firmware, see `nexus doctor` (planned) for the NIC checks.

## Remote Ctrl+Alt+Del and system keys

In the native viewer, **Ctrl+Alt+Shift+D** sends Ctrl+Alt+Del to the target. The input stream still
requires its existing input grant, authentication, step-up and consent. The SYSTEM child signals its
service-only event; the service impersonates the session token and calls
[Microsoft's SendSAS API](https://learn.microsoft.com/en-us/windows/win32/api/sas/nf-sas-sendsas).
A normal user agent cannot perform this operation. Win, Alt+Tab, Alt+Esc, Alt+F4 and Ctrl+Esc are
captured while a Windows viewer is foreground; local viewer escape shortcuts remain available.

The MSI enables `HKLM\Software\Microsoft\Windows\CurrentVersion\Policies\System\SoftwareSASGeneration`
with DWORD `1` (Services) only if it was absent. Existing policy is preserved. A configured `0` or
`2` refuses SAS; `1` and `3` permit the service. On managed machines the policy owner must allow
Services under **Windows Components → Windows Logon Options → Disable or enable software Secure
Attention Sequence**. The MSI never overwrites this policy and leaves it intact on uninstall.
UAC remains enabled and credentials are still entered by the authorized controller at Windows login.

## Installer source

`installers/windows/build-msi.ps1` authors the service/CLI MSI with embedded binary manifests.
Its optional `-HardwareFfmpeg` switch includes the existing GPU encoder feature and requires a
matching FFmpeg SDK at build time and DLLs in the installed service's runtime search path. The
script has been edited but not executed during this update.

## First-run checklist on a real Windows PC (what would turn 🟡 into ✅)
1. `msiexec /i Nexus-Remote.msi` → `sc query NexusAgent` is RUNNING; `Get-Process nexus-agent` shows two processes
   (session 0 and the console session, the latter as SYSTEM).
2. `C:\ProgramData\Nexus` is unreadable for a non-admin user.
3. Pair from another device; `nexus remote screenshot` returns the desktop.
4. Trigger a UAC prompt (`Start-Process cmd -Verb RunAs`): the controller's picture follows it and a click on **Yes**
   works. Lock the PC (Win+L): the lock screen is visible, and typing the password remotely unlocks it.
5. Fast-user-switch to another account: the picture follows within ~2 s.
6. `Stop-Service NexusAgent`: both processes disappear; `Start-Service` brings them back.
7. Kill the child (`taskkill /f /im nexus-agent.exe` on the SYSTEM copy): it returns after ~1 s.
