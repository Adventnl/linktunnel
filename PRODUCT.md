# Link Tunnel

<!-- impeccable:product-schema 1 -->

## Platform

web

## Product Purpose

Control paired computers through one desktop app or the CLI. Connecting should keep the device list, remote desktop, terminal and session controls together.

## Operating Context

Existing Tauri 2 desktop shell with React/TypeScript, Rust agents and a native wgpu viewer. Windows is the immediate implementation and verification platform. Existing macOS and Linux support must retain working fallbacks.

## Capabilities and Constraints

Preserve authenticated local agent IPC, device identity, pairing verification, consent, lockdown, clipboard, audio, remote input, terminal and phone controls. Video stays in a native surface. English and Simplified Chinese catalogs remain in parity. CLI access must not require the dashboard.

## Brand Commitments

The user requested macOS Liquid Glass materials, integrated window chrome, consistent icons, purposeful animation and Sill as a behavioral reference. The local reference is ../sill; implementation should translate useful patterns into Rust rather than transplanting Apple APIs.

## Product Principles

- Keep connection and control in one workspace.
- Make destructive device actions deliberate and discoverable in a device menu.
- Prefer a current picture and readable text over a queue of stale frames.
- Expose operations through the CLI as well as the desktop app.

## Open Decisions

The user confirmed light glass as the default appearance. Dark and system remain selectable. The audience is inferred from the request to be the owner controlling their own paired computers; broader audience requirements are unconfirmed.
