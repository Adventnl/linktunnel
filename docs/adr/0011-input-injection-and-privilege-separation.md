# ADR-0011: Input injection and privilege separation

**Status:** Accepted (design; Phases 3, 5, 6)

## Decision
* **Injection:** Windows `SendInput` (+ SYSTEM helper on `Winsta0\Winlogon` for UAC/lock
  screen); macOS `CGEvent` (Accessibility permission); Linux `uinput` (+ XTest fallback).
* **Process split.** The long-running, network-facing **agent** runs unprivileged (a dedicated
  service account on Linux/macOS; a restricted service on Windows). A minimal **session
  helper** holds the privilege needed to touch the secure desktop or another user's session and
  exposes a tiny RPC (`inject(event)`, `capture(monitor)`, `desktop_info()`), authenticated by
  OS peer credentials (named-pipe ACLs / Unix-socket `SO_PEERCRED` / `LOCAL_PEERCRED`) and
  accepting only already-validated, size-bounded messages.
* **No privilege from the network:** nothing a remote peer sends can select what runs in the
  helper; the helper never parses network data.
* **Safety:** `ReleaseAll` is sent on focus loss and disconnect so a dropped session never
  leaves a modifier stuck down; the privacy-screen and panic hotkey live in the helper so they
  work even if the agent hangs.

## Honest limits (see LIMITATIONS.md)
macOS cannot capture or inject at the login window from a user-level app; Secure Input
fields suppress synthetic keystrokes; DRM-protected video is blanked by the OS.
