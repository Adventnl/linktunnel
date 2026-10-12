# nexus-privacy

Windows physical-monitor shield and optional local-input blocking, scoped to a session lease.
Local Esc, timeout, secure desktop, disable and disconnect restore access. Other platforms return
unsupported. See [ADR 0022](../../docs/adr/0022-physical-screen-privacy.md).

Viewer toggle: Ctrl+Alt+Shift+B. Pairing needs the `privacy` grant; existing pairings must be renewed
with that grant. Test lifecycle: `cargo test -p nexus-media --test privacy_e2e`. Cross-check the FFI:
`cargo check -p nexus-privacy --target x86_64-pc-windows-msvc`. Actual DXGI/display-driver validation
is still required on Windows; the lifecycle test uses a recording backend.
