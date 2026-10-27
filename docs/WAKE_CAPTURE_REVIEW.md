# Code Cleanup Report — Windows wake-to-capture path

## Scorecard
| Category | Rating |
|---|---|
| Architecture & layering | Pass |
| File size & granularity | Pass |
| DRY & reuse | Pass |
| Hardcoding / magic values | Pass |
| Types & correctness | Needs work |
| Async & error handling | Needs work |
| State management | Needs work |
| Performance & complexity | Pass |
| Density & clarity | Pass |
| Naming & readability | Pass |
| Security | Pass |
| Tests & quality gates | Needs work |

Overall: the capture path had a credible post-wake stall; the repair passes Windows unit
and lint checks, but production acceptance requires reproducing sleep/wake with both devices.
Ratings describe the inspected path before remediation, not the entire repository.

## Findings
### P0 — Critical
- None found in this targeted review.
### P1 — High
- `crates/nexus-capture/src/windows_capture.rs:493` — DXGI timeouts returned no frame
  indefinitely, while empty-surface recovery ran only on the initial acquisition. Why:
  a waking display can be dark for both DXGI and GDI at first. Fix: retain recovery state
  and probe GDI with a one-second throttle until it produces a picture.
- `crates/nexus-capture/src/windows_capture.rs:430` — capture made no display power request.
  Why: waking the PC does not establish that Windows has resumed the display. Fix: reset
  the display idle timer on the capture thread, without a persistent power override.
### P2 — Medium
- None added to scope.
### P3 — Nits
- Updated stale module verification wording to distinguish unit checks from device acceptance.

## Remediation plan
1. Implemented display pulse and timeout recovery while preserving protected-content behavior.
2. Added two deterministic recovery-state tests; all eight capture unit tests pass on Windows.
3. Strict capture Clippy and changed-module rustfmt pass. Whole-workspace rustfmt check is
   blocked by pre-existing CRLF files throughout the checkout; no unrelated files were reformatted.
4. Prepared the locked release agent/CLI build and a backed-up agent update. Administrator
   installation and the live Mac wake/reconnect/UAC checklist remain pending.

Inspected → Windows capture initialization and frame recovery; issues found → 2 P1;
fixed → both code paths; deferred → live acceptance and administrator install;
files changed → capture source/Cargo feature plus setup/documentation;
tests added → 2; gates → `cargo test --locked -p nexus-capture --lib` 8 passed,
`cargo clippy --locked -p nexus-capture --all-targets -- -D warnings` passed,
`rustfmt --check crates/nexus-capture/src/windows_capture.rs` passed;
limitations → hardware behavior unverified; no unrelated features added.
