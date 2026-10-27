# Android Codex workflow

## What this delivers

Pick a repository on the PC, send a prompt, read agent messages and command output, inspect
the turn's diff, answer questions, approve individual actions, stop work, and send follow-ups.
Codex runs against the real PC working directory, under the signed-in Windows user. VS Code
does not need to be visible or running. These are separate conversations from the VS Code
extension; they edit the same files. Avoid simultaneous agents editing the same working tree.

The phone has a native, text-first interface. It receives no screen stream and has no desktop
input, clipboard, terminal or arbitrary-RPC endpoint. The Codex agent can run commands and
edit files as part of a task, subject to its workspace sandbox and explicit approvals.

## Use on this PC

The development APK is `dist/linktunnel-debug.apk` (Android 8 or later). It is signed with
a local development key, suitable for personal sideloading, not a Play Store release.
The PC host has been configured at `%LOCALAPPDATA%\Nexus\mobile`, is listening on its
Tailscale address on port 9443, and has a current-user logon startup shortcut.

1. In **Administrator PowerShell**, from the repository root, run:

   ```powershell
   ./installers/windows/finish-mobile-setup.ps1 -UpdateDesktopHost
   ```

   This adds a firewall rule restricted to the Tailscale adapter, its local address and
   Tailscale IPv4 sources. It also backs up and replaces the installed desktop agent
   with `target/release` builds, then restarts `NexusAgent`, preserving configuration and
   pairings. The current agent must be built first; use the build command below.
   Omit `-UpdateDesktopHost` to configure mobile access only.

2. Install the APK on Android, and connect Android to the same Tailscale network as the PC.
3. Open `%LOCALAPPDATA%\Nexus\mobile\pairing.json` on the PC and privately transfer its
   complete JSON to the phone. Paste it into linktunnel and tap **Connect to PC**.
4. Choose `git-monitor` (or another configured repo) and send a task. Return through
   **Conversations** to check progress or continue an earlier task.

Pairing JSON contains a bearer credential. Do not commit it, publish it or attach it to an
issue. Android encrypts it with Android Keystore and excludes app data from backups.
HTTPS verifies the exact PC certificate supplied out of band, including its validity period.
The development certificate expires after one year. Re-pair after rotation.

The PC must be awake and the Windows user must have signed in. Locking Windows is fine.
While a task is active or waiting for input, a user-session Windows power lease prevents idle
system sleep without turning on the display. It releases when work ends or the bridge exits.
The mobile bridge starts at user logon, not before logon. Phone disconnects and app closure
do not stop running tasks. A host/Codex process restart interrupts active work; recorded
conversations remain and a follow-up resumes the saved Codex thread.

## Fresh setup and configuration

Install Node 22+, the npm Codex CLI, Git for Windows (for OpenSSL), and Tailscale. Run
`codex login` as the Windows user who owns the repositories. From an ordinary PowerShell:

```powershell
./installers/windows/setup-mobile-host.ps1 -Repositories git-monitor,linktunnel -Start -InstallStartup
```

Without `-Repositories`, setup allows the immediate Git repositories under
`$env:USERPROFILE\Documents\Github`. Set `-RepositoryRoot` to use another parent. The phone
can select only configured IDs, never arbitrary paths. Canonical paths must remain inside
the configured root, including after resolving links. Change `mobile-host.json` locally
and restart the host to change the allowlist.

Setup preserves existing configuration and pairing unless `-RotatePairing` is supplied.
Stop the running mobile host before rotating, then restart it and re-pair the phone.
Rotation creates a new credential and certificate; existing phones lose access.

For foreground diagnosis:

```powershell
node apps/nexus-codex-host/src/main.mjs "$env:LOCALAPPDATA\Nexus\mobile\mobile-host.json"
node apps/nexus-codex-host/test/probe.mjs "$env:LOCALAPPDATA\Nexus\mobile\mobile-host.json"
```

Only one host may listen on the configured port. Background logs are `host.log` and
`host-error.log` in the data directory. Raw Codex diagnostics and credentials are not logged.
To disable the feature, stop its `node.exe` process and remove
`linktunnel Host.lnk` from the current user's Startup folder. Remove its named firewall
rule in Administrator PowerShell, and delete the phone's pairing using **Forget this PC**.

## Build and checks

The Android build uses the official command-line tools directly, with no Gradle/plugin or
runtime dependencies. Install a JDK 21, Android SDK platform 35 and build-tools 35.0.0:

```powershell
sdkmanager 'platforms;android-35' 'build-tools;35.0.0'
./apps/nexus-android/build.ps1 -Sdk $env:ANDROID_HOME -Jdk $env:JAVA_HOME
node --test apps/nexus-codex-host/test/host.test.mjs
```

The build compiles resources and Java, runs D8 and zipalign, signs the APK, and verifies its
signature. Keep `target/nexus-android/development.p12` locally to install later builds over
the current development build. CI generates its own disposable development key; a CI APK
cannot update a locally signed installation without uninstalling the old app first.

The optional real-runtime smoke gate creates a file and edits it in a follow-up using the
existing Codex account in a disposable directory:

```powershell
node apps/nexus-codex-host/test/live-smoke.mjs "$env:APPDATA\npm\node_modules\@openai\codex\bin\codex.js"
```

Build the desktop repair using Visual Studio C++ Build Tools, Strawberry Perl and NASM:

```powershell
. ./scripts/windows-build-env.ps1
cargo build --locked --release -p nexus-agent -p nexus-cli
cargo test --locked -p nexus-capture --lib
cargo clippy --locked -p nexus-capture --all-targets -- -D warnings
```

## Wake-to-black repair

The capture thread now periodically resets Windows' display idle timer while streaming.
It also remembers an unprotected black initial DXGI surface and rechecks GDI once per second,
including during DXGI timeouts. Previously it checked only that first frame; when both
capture methods initially saw a dark waking display, later timeouts could leave it black.
Fallback switches only after GDI sees actual pixels. Protected-content frames disable
this recovery. No persistent display power override remains after streaming stops.

This is a plausible fix for the reported symptom, not a confirmed diagnosis from a live
reproduction. After installing the repaired desktop host, test: sleep PC → wake from Mac →
connect without touching PC input → verify picture; then disconnect/reconnect, lock/unlock,
and check UAC. A monitor-less hardware configuration can still require a virtual display.

## Verification and remaining work

Verified on this Windows PC: Android APK compilation and v2/v3 signature validation;
13 host unit/integration tests on Windows (12 portable tests and a Windows power-lease check);
existing Codex sign-in, new thread, real file creation and follow-up edit, streamed
agent replies and completed status; HTTPS certificate/pairing-pin consistency, rejected
unauthenticated calls and authenticated repository listing; 8 capture unit tests, live
capture of the awake 1920×1080 Windows desktop/cursor, strict capture Clippy and formatting
of the changed Rust module.
The locked Windows release build of the agent and CLI also passed; the repaired agent is
ready at `target/release/nexus-agent.exe` for the administrator installation command above.

Not verified: installation/rendering on a physical Android device, background/resume and
certificate rejection on Android hardware, phone-to-PC access through Windows Firewall,
and live Mac-to-PC sleep/wake. No phone is attached to ADB. The existing installed desktop
service is unchanged until the administrator update command is run.

First version boundaries: 100 conversations retained by the bridge; last 200 items and a
bounded text tail/diff per conversation; no push notifications, phone Wake-on-LAN relay,
attachments, model/settings picker or attachment to the exact active VS Code conversation.
Long/full history remains in Codex's local thread store. Other MCP elicitation/permission
request types fail explicitly rather than hanging or being silently approved. A sleeping PC
cannot receive a prompt until woken using the existing Mac workflow or an always-on helper.

## Architecture and follow-on plan

```text
Android native UI + Keystore pairing
    → pinned HTTPS over Tailscale
    → linktunnel host (ordinary Windows user, repository allowlist)
    → private codex app-server stdio
    → PC repository + Codex account + sandbox/approvals
```

1. Implemented: repo/chat/task API, native APK, private pairing, persistence, questions,
   individual approvals, cancellation, follow-ups and CI build gates.
2. Administrator handoff: install desktop repair and scoped firewall rule.
3. Device acceptance: install/pair phone, test a small `git-monitor` change and follow-up,
   go offline mid-task then reconnect, decline an approval, rotate pairing and reject the
   old phone credential, then run the Mac sleep/wake checklist.
4. After acceptance: consider notifications, optional always-on wake helper and existing
   VS Code conversation attachment. Do not add desktop streaming to the phone workflow.

The integration uses [Codex app-server](https://learn.chatgpt.com/docs/app-server), with the
installed CLI's generated schemas checked during implementation. It inherits the PC's
Codex account/configuration; it does not copy account credentials to Android or call a
separate hosted execution service. Windows display power behavior follows
[SetThreadExecutionState](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-setthreadexecutionstate).
