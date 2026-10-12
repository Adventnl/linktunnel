# ADR-0018: Windows service and secure-desktop control

**Status:** Accepted (design and code complete; **not executed on Windows**)

## Context
Remote control must work on the UAC prompt, the lock screen and the sign-in screen (the secure desktop), survive fast user switching
and start at boot. See [WINDOWS.md](../WINDOWS.md) for the full analysis.

## Decision
1. A **LocalSystem service** (`nexus-agent --windows-service`) supervises **one child: the whole agent**, started with the service's own
   SYSTEM token retargeted at the active console session (`TokenSessionId`) on `winsta0\default`.
2. Capture and input **threads follow the input desktop** (`OpenInputDesktop` → `SetThreadDesktop`) and rebuild the DXGI duplication on a switch.
3. The supervisor is a portable state machine (tested on Linux): replace the child when the console session changes; back off on crashes.
4. **No terminal in the SYSTEM service** by default (`--no-terminal`): a shell there would be SYSTEM.
5. Attended consent is a `WTSSendMessage` Yes/No box (a SYSTEM agent's IPC is admin-only); it cannot appear on the secure desktop, so a
   locked PC answers by timeout (refuse). The lock screen is for **pre-authorised** devices.
6. The state directory is locked to SYSTEM + Administrators by ACL on every start and by the MSI.

## Consequences
* One agent per session instead of a thin helper: a QUIC session cannot be handed between processes, and a second protocol (service ↔ helper)
  would double the attack surface. Cost: presence flaps for ~1 s on a session switch.
* RDP: a connected RDP user detaches the console's display; capture then fails until it returns (WINDOWS.md).
* Everything Win32 is `cargo check`/`clippy` clean for `x86_64-pc-windows-msvc` and **unrun**; the CI `windows-latest` jobs are its first compile of the full agent.
* Code signing is not required for SYSTEM to reach `Winlogon`; it is for distribution (SmartScreen).

## Alternatives
A helper process per session speaking IPC to the service (more moving parts); UIAccess (needs a signed binary in a trusted path and still cannot
capture the secure desktop); using RDP/Windows' own shadowing (not ours to control, not cross-platform).
