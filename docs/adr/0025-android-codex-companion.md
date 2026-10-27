# ADR-0025: Android Codex companion over a user-session bridge

Status: accepted; device acceptance pending.

## Context

The mobile use case is sending coding tasks to PC repositories. A scaled desktop stream
makes editor text and interaction too small. The existing desktop capture service runs as
SYSTEM, whereas Codex credentials and repositories belong to an interactive Windows user.

## Decision

Build a native Android text client and a dependency-free Node 22+ companion host. Run the
host under the repository owner's user account, separately from the SYSTEM remote desktop
service. Use private Codex app-server stdio to preserve the real agent harness, local files,
instructions, threads and approvals. Use pinned HTTPS on the PC's Tailscale IPv4 address.
Provision an independent random credential and certificate via a private pairing file.

The phone API exposes repository IDs and conversation operations only. It never accepts
raw RPC method names, filesystem paths, shell command strings or desktop input. Codex
starts threads with workspace-write sandboxing and on-request approval. Supported server
questions and command/file approvals are represented explicitly. Unknown interactions are
rejected. Persist bounded task views atomically; preserve Codex thread IDs for resumption.

## Consequences

Android can run normal Codex tasks without a monitor or VS Code UI. The existing native
desktop agent/protocol remain independent. The phone needs Tailscale and private pairing;
Windows needs a scoped firewall rule. Sign-in is required after boot for the user bridge.
This creates separate conversations in the same repository, not a takeover of the active
VS Code extension session. Wake from a fully sleeping PC needs a separate always-on sender.

The APK is a personally sideloaded development build. No Play Store distribution, release
signing or physical-device validation is claimed. See [setup and evidence](../ANDROID_CODEX.md).

## Alternatives

An Android port of the full QUIC/video desktop controller brings capture/input concepts the
workflow does not need. A mobile web page avoids an APK but does not provide the requested
native package and Keystore pairing. Automating the VS Code UI would depend on an unlocked
desktop and fragile coordinates; app-server gives the conversation lifecycle directly.
