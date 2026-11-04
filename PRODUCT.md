# Link Tunnel

<!-- impeccable:product-schema 1 -->

## Platform

web

## Product Purpose

Control paired computers through one desktop app or the CLI. Keep the computer list, desktop, terminal and session controls easy to reach; a desktop may occupy the workspace or its own window.

## Operating Context

Existing Tauri 2 desktop shell with React/TypeScript, Rust agents and a native wgpu viewer. Windows supports embedded native desktop sessions with live pop-out and return. macOS and Linux use a separate native viewer. Ordinary viewer windows follow the remote screen proportions; fullscreen may letterbox. The interface is designed at 1920×1080 with responsive fallbacks.

## Capabilities and Constraints

Preserve authenticated local agent IPC, device identity, pairing verification, consent, lockdown, clipboard, audio, remote input and terminal controls. The desktop shell removes phone, command-search, CLI-help and local-machine status utilities; underlying CLI and phone capabilities remain independent. Lockdown belongs in Settings. Video stays in a native surface. English and Simplified Chinese catalogs remain in parity. CLI access must not require the dashboard.

## Brand Commitments

The user requested Liquid Glass across the application in white/light and dark modes, a gradient fade when switching appearance, integrated window chrome, consistent icons and purposeful animation. Tailscale account context and Settings belong in the compact top bar. Sill remains a behavioral reference. The local reference is ../sill; implementation should translate useful patterns into Rust rather than transplanting Apple APIs.

## Product Principles

- Keep connection controls together while allowing a desktop to lift into its own window.
- Make destructive device actions deliberate and discoverable in a device menu.
- Prefer a current picture and readable text over a queue of stale frames.
- Expose operations through the CLI as well as the desktop app.

## Open Decisions

The user confirmed light glass as the default appearance. Dark and system remain selectable. The audience is inferred from the request to be the owner controlling their own paired computers; broader audience requirements are unconfirmed.
