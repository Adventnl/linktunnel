# ADR-0016: Virtual display for headless Linux servers

**Status:** Accepted (Linux, verified)

## Context
A server with no monitor has no X server and nothing to capture, yet the brief wants a GUI session "on a headless Linux box with
no monitor and no GPU".

## Decision
`nexus-vdisplay` starts a private, TCP-less **Xvfb** on a free display number and supervises an optional session command (window
manager, desktop). The existing X11 capture (XDamage) and XTest input backends then work unchanged, and encoding is software.
* Display numbers are probed with a process-wide counter and ownership is confirmed through the lock file's PID (two concurrent
  starts, and an X server that lost the race, are handled; tested).
* Teardown is SIGTERM-then-kill and removes the socket and lock file a killed server cannot.
* The agent creates one when no real screen is capturable (`[display] virtual = auto|always|never`); `always` gives a remote user
  a private desktop that does not disturb the monitor.
* The session command restarts with 0.5 s→30 s back-off.

## Consequences
* ✅ `scripts/demo-server.sh` (and a CI job) shows the full headless story end to end.
* Xvfb has no GPU: encoding is CPU-only; a heavy desktop will be slow.
* **Not** a Wayland solution: a headless Wayland compositor (`weston`/`sway` headless) plus PipeWire capture is the planned
  upgrade; `uinput` input likewise.
* Security: the X socket is protected by filesystem permissions and X's default same-user rule; the virtual desktop runs as the agent's
  user, so a `terminal`/`screen`+`input` grant on a server is as powerful as that account (see the systemd hardening notes).

## Alternatives
Xvnc / x11vnc (a VNC server we would then not control the protocol of); Xorg dummy driver (needs root and a driver stack); a pure-Rust
framebuffer compositor (large, unnecessary for the goal).
