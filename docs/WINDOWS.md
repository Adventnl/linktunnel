# Windows: service, secure desktop, UAC and the lock screen

> **Verification status.** Everything here is written against the Win32 API and **type-checked and
> linted for `x86_64-pc-windows-msvc`** from Linux; the decision logic (`nexus-winservice::supervisor`,
> command-line quoting) is unit-tested on every platform. **None of it has been run on a Windows machine
> yet.** The first real test is the checklist at the end of this page. [STATUS.md](STATUS.md) repeats this.

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
2. **Following the desktop** (`nexus-desktop`): before each capture attempt (at most 5×/s) and each input event
   (at most 10×/s) the thread asks `OpenInputDesktop` for the desktop that has the user's attention, and if its
   name differs from the one it is attached to it `SetThreadDesktop`s to it. Capture then rebuilds the
   Desktop Duplication (which is bound to a desktop) and emits a full frame. When the thread is **not** SYSTEM,
   attaching to `Winlogon` fails with *access denied*; the code keeps working on the desktop it has (which goes
   black during UAC; that is a Windows rule, not a bug).
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
| State folder | SYSTEM + Administrators only | it holds the device identity key; the service re-applies the ACL (`icacls /inheritance:r`) on every start, and the MSI sets it too |
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
* The panic hotkey / "Disconnect" indicator for a SYSTEM agent is a Phase 9 item (see LIMITATIONS.md).

## Known limits (honest list)
* **Protected content** (DRM video) is black in Desktop Duplication, by design of Windows.
* **RDP**: when someone connects over Remote Desktop, the console session is disconnected and its display driver is
  detached; capturing it then fails until it reconnects. The supervisor follows the console-session change, but a
  disconnected console has nothing to show. Controlling an *RDP* session needs the agent in that session (not done).
* **Multiple monitors with mixed DPI**: positions are physical pixels (the agent manifest must declare per-monitor
  DPI awareness; the MSI does not yet embed that manifest, tracked in STATUS.md).
* **HDR**: the duplication returns the tone-mapped SDR image Windows composes; no HDR passthrough.
* **Windows Server Core** has no desktop: the agent offers terminal/files/ports only (the virtual-display path is Linux-only).
* **Modern Standby**: presence reports sleep via power events; Wake-on-LAN from Modern Standby depends on the NIC and
  OEM firmware, see `nexus doctor` (planned) for the NIC checks.

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
