# nexus-auth

Server-side, single-use WebAuthn/TOTP verification and the ephemeral browser bridge.
See [step-up setup](../../docs/STEP_UP.md) and [ADR 0020](../../docs/adr/0020-session-stepup.md).

`cargo test -p nexus-auth` verifies enrollment, signatures, session/device binding, expiration and
persistent replay rejection. The dev-only software authenticator never enters production builds.
`cargo build -p nexus-auth --example passkey_smoke` and `node scripts/test-passkey-browser.mjs`
exercise the actual HTML bridge in Chrome with a temporary virtual authenticator.
