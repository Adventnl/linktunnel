# ADR 0022: fail-safe, session-scoped physical privacy

Accepted 2026-10-12.

Privacy needs an explicit pairing grant (`privacy`) and attended consent where applicable.
A backend creates a lease; disable, session termination, lockdown/revocation (which terminate
sessions) and process exit restore access. Only one session can own the physical shield.

Windows: a dedicated message thread owns a topmost black shield spanning the virtual desktop.
WDA_EXCLUDEFROMCAPTURE excludes it from supported capture APIs. Layered hit-test transparency
preserves remote clicks on underlying windows. Low-level input hooks optionally reject physical
keyboard/mouse events but permit injected events. Physical Esc restores the screen, while remote
Esc is forwarded. A visible restore message remains on the physical shield. A 10-second lease
watchdog requires a renewal each second; secure-desktop transitions end the lease. Creation is
transactional, and every hook/window is cleaned up on failure. Restoration is reported to the viewer.

This is a Windows 10 2004+ normal-desktop implementation. It must be tested with real DXGI capture
and display drivers before claiming privacy in production. Hooks are not a secure input boundary:
OS secure attention and drivers/raw-input paths remain OS-controlled. Never obscure the UAC/login
secure desktop. macOS and Linux currently refuse privacy explicitly; no generic black overlay
that would hide the remote picture is advertised as a privacy backend. A private Xvfb desktop is
an alternative on Linux, not blanking an existing physical desktop.
