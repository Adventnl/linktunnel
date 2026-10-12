# macOS live-control verification — 2026-10-12

Host: macOS, Apple Silicon arm64. Implementation changes are in the working tree; this is not
an immutable released-build benchmark. Synthetic 640×360 content through real loopback QUIC,
VideoToolbox encoding and OpenH264 decode, measuring flash-to-decoded-picture in the test harness:

```sh
cargo test -p nexus-media --test e2e video_flows_and_a_flash_is_seen_with_measured_latency -- --nocapture
```

Observed eight flash changes: p50 **4.42 ms**, maximum **18.84 ms**. No p95/p99 claim from eight
samples. This does not include a physical capture sensor or native surface presentation; it does
not establish input-to-photon, real LAN/WAN, 1080p/4K, 60/120 fps or resource budgets.
The wgpu shader was separately executed through Metal and read back for pixel colour, orientation,
alpha and letterbox validation. No GPU window-present timing was measured.

The full Mac workspace test run passed **583 Rust tests**, with one existing environment-specific
ignored test. The optional FFmpeg codec run passed 22 tests with the vendor-GPU lab test ignored;
the VideoToolbox encode/decode test executed successfully. All 22 two-agent integration tests pass.
Standalone binary TLS/admission/relay-only traffic and privacy lifecycle recording tests pass.
Chrome WebAuthn enrollment/authentication with a temporary virtual authenticator passed; browser
screenshots were inspected at desktop and mobile sizes. Frontend: 20 tests, typecheck and build pass.
Workspace strict clippy, optional FFmpeg clippy, Windows privacy cross-clippy, rustfmt, cargo-deny,
threat-model traceability and relay Compose configuration validation pass.

Tauri backend tests could not start because its separately locked tauri 2.12.3 could not be fetched
(DNS failure) and is absent from the local offline registry cache. Its only app-backend change is
adding the new privacy grant to the default pairing list. Docker Compose was not deployed: no
Docker daemon was running. Native Windows GPU/privacy/UAC/login and physical cross-device desktop
control remain mandatory acceptance tests in FULL_CONTROL_QA.md.
