# ADR 0020: session-bound passkeys and TOTP

Accepted 2026-10-12. Extends ADR 0006.

The agent already authenticated device keys, but `security.unattended_requires_stepup` was ignored.
Enforce it before invoking the target handler, so screen, input, clipboard, files, terminal, power
and forwarding cannot race authentication. Attended consent remains a separate gate.

Use webauthn-rs 0.5.5 (user verification required) and totp-rs 5.7 (HMAC-SHA1, six digits, 30 seconds,
± one step). Enrollment happens through authenticated local IPC and must prove possession.
Secrets and credential counters live in the agent's existing SecretStore: OS keychain for normal
interactive installs, protected files for services/portable installs. Corrupt/unreadable state
fails startup. Verification and replay-floor persistence must succeed before access is granted.

WebAuthn challenges stay in target memory, bound to both Hello nonces and the authenticated device
key. Every attempted response consumes the challenge, including failures. TOTP's last accepted
step persists across restarts. Peer session limits and refused-proof lockout bound guessing.

An ephemeral localhost browser bridge calls WebAuthn (primary) and accepts a TOTP fallback. The RP
is localhost, permitting ephemeral ports. Bridge routes require a random 256-bit fragment token,
an exact Host, and POST Origin; no CORS, no permanent listener, no external assets. A challenge
or assertion is never a reusable access token. `auth unlock` keeps the *exact approved connection*
for the next local command, once, for at most 60 seconds. Pending connections expire actively.
The viewer opens this prompt automatically on the dedicated step-up-required IPC error.

Tradeoffs: this is device-local session step-up, not a signaling account/passkey login service.
Enroll a credential usable on the controller (synced passkey or portable security key); a passkey
bound only to the target cannot authenticate from a different computer. WebAuthn verification
uses webauthn-rs's vendored OpenSSL dependency; transport continues to use rustls. Document this
exception to the old OpenSSL dependency ban rather than implement our own WebAuthn crypto.
Fresh connections, including unattended reconnects after a lost transport, require a fresh factor.
