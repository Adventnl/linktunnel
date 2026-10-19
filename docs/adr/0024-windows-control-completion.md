# ADR 0024: Windows session control and secure attention

**Status:** Accepted; implementation only, no build or testing performed for this revision.

## Context
The existing Windows service could launch an interactive SYSTEM child, but normal backend detection
never selected a Windows injector. Desktop duplication was initialized before reaching its capture
thread; access loss ended video permanently, startup retries reused a missing session, and keyboard
injection could not produce the secure attention sequence. Touch/pen and embedded DPI awareness were
missing. The Tauri application alone provided connection visibility and panic controls.

## Decision
* Select the native Windows injector through `default_injector`. Construct capture and input resources
  on their dedicated threads, attach to the current input desktop before using them, and release
  desktop-bound resources before switching. Failed desktop attachment refuses input.
* Keep the existing authenticated/granted input stream. Detect Ctrl+Alt+Del from that stream's held
  Ctrl/Alt keys and ask the SCM service through a random, SYSTEM-only auto-reset event. The service
  impersonates its child's session token and calls System32 `SendSAS(FALSE)`. No credentials, consent,
  session grants or UAC settings are changed by this operation.
* The installer enables service-generated SAS only when no policy exists. Existing policy is preserved;
  the input backend refuses SAS unless policy explicitly allows Services. This machine-wide policy is
  permanent in the MSI and never overwritten; uninstall leaves it for the administrator to manage.
* Select the active physical console, otherwise one existing active RDP session. Re-enumerate sessions
  on notifications and retry; never create additional Windows sessions. Put each child in a
  kill-on-close job before resuming it so supervisor termination also kills privileged descendants.
* Rebuild capture after access/device loss. Use an attached-desktop GDI fallback when DXGI is absent,
  including RDP virtual displays. Rotate duplication pixels into physical display orientation.
  Announce monitor-layout changes and refresh input/cursor geometry.
* Embed PerMonitorV2 awareness in MSVC agent/controller binaries. Display DPI is metadata; Windows
  pointer/cursor geometry remains physical pixels.
* Use Windows synthetic pointer devices for ten touch contacts and one pen with pressure/tilt,
  canceled on focus loss, disconnect and desktop transitions.
* Intercept Win, Alt+Tab/Esc/F4 and Ctrl+Esc only while the native Windows viewer is foreground.
  Ctrl+Alt+Shift+D sends remote Ctrl+Alt+Del, accounting for optional Cmd/Ctrl mapping.
* A separate target UI thread follows desktops and handles physical Ctrl+Alt+Shift+Esc. Panic closes
  sessions and enables lockdown independently of Tauri. The hook is destroyed before the thread
  attaches to another desktop. The floating active-session badge was subsequently removed because
  it obscured remote content and intercepted clicks; ordinary disconnect remains in the dashboard
  and CLI.
* Replace the complete state tree DACL with SYSTEM + Administrators, refusing reparse points and
  aborting startup if ACL protection fails.

## Consequences
An RDP/user-session transition still replaces the whole child and reconnects the encrypted session.
GDI fallback costs CPU and can include privacy shields in the remote image; DXGI remains preferred.
Protected video is subject to Windows capture restrictions. HDR passthrough, kernel/virtual-display
privacy and GPU zero-copy/hardware decoding are outside this control-path revision. Ordinary user
processes still cannot control Winlogon. Terminal/file/audio capabilities retain their existing
grants and configuration.

No test counts, compile/lint claims, latency claims or physical-device success claims are added.

## References
* [SendSAS and service impersonation](https://learn.microsoft.com/en-us/windows/win32/api/sas/nf-sas-sendsas)
* [SetThreadDesktop restrictions](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setthreaddesktop)
* [Synthetic pointer devices](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-createsyntheticpointerdevice)
* [InjectSyntheticPointerInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-injectsyntheticpointerinput)
