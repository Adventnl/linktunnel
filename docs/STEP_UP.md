# Passkeys and TOTP for unattended control

`security.unattended_requires_stepup = true` is now enforced (the default). Enroll on the **target**:

```sh
nexus auth enroll-passkey
# Optional fallback:
nexus auth enroll-totp
nexus auth status
nexus trust unattended 'MacBook'
```

For a machine-wide agent use `nexus --system auth ...` from the service's authorized administrator
context. For portable installs use the same `--home` as the agent. The passkey must be available
to the controller too: use a synced credential or a portable security key. TOTP enrollment prints
an otpauth URI and asks for a code; no factor is activated before possession is proved.

On the controller, `nexus remote view 'Windows PC'` automatically opens the browser verification
prompt, offering a passkey or an enrolled authenticator code. The Tauri dashboard launches this
same native viewer. Remote command-line operations can approve their next connection explicitly:

```sh
nexus auth unlock 'Windows PC'
# or: nexus auth unlock 'Windows PC' --totp
nexus remote view 'Windows PC'
```

Approval preserves one exact encrypted connection for one subsequent local command, for 60 seconds.
It does not change device grants or grant a reusable token. A code cannot be used twice (including
the enrollment code) even after restarting the agent. Wait for a new code after enrollment.
Unattended targets with no enrolled factor refuse access. Old controllers that cannot negotiate
step-up are refused when it is required. Attended consent and paired identity checks remain.

Fresh unattended connections, including reconnects after transport loss, require a factor.
This is device-local step-up rather than account login on the signaling service. A headless
administrator can enroll TOTP locally; WebAuthn enrollment requires a browser/authenticator on
that administrator's machine and authenticated IPC access to the target's agent. Factor removal
and recovery UI are not provided yet; protect and back up the service's private credential store.
