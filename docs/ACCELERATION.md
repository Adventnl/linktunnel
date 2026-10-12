# Hardware encoders and the native viewer

macOS selects VideoToolbox first. No FFmpeg is needed for that path. The viewer uses native
wgpu on Metal/D3D12/Vulkan, with CPU decoded BGRA uploaded to a texture and GPU scaling.
`NEXUS_SOFTWARE_VIEWER=1 nexus remote view DEVICE` forces the CPU presenter.

For Windows/Linux vendor encoders, install an FFmpeg SDK exposing libavcodec, libavformat,
libavutil and libswscale, plus matching drivers. Build:

```sh
cargo build --release -p nexus-agent --features hardware-ffmpeg
cargo build --release -p nexus-cli -p nexus-relay
```

On macOS (to compile/test the optional shared backend): `brew install ffmpeg pkgconf`.
On Debian/Ubuntu: `sudo apt-get install pkg-config clang libavcodec-dev libavformat-dev libavutil-dev libswscale-dev`.
On Windows, provide an MSVC-compatible FFmpeg SDK via `FFMPEG_DIR` (include/ and lib/), with its
DLLs on PATH. See the [binding build instructions](https://github.com/zmwangx/rust-ffmpeg/wiki/Notes-on-building).
An executable-only `ffmpeg.exe` installation is insufficient for linking this build feature.

Backend order: macOS VideoToolbox, then feature-enabled NVENC → AMF → Quick Sync → VAAPI, then
OpenH264. The opening probe must emit an immediate IDR with SPS/PPS and matching timestamps.
The viewer HUD reports the selected encoder. Run on matching hardware:

```sh
cargo test -p nexus-codec --features hardware-ffmpeg hardware_stream_decodes_and_changes_rate -- --ignored --nocapture
```

Current limits: H.264 only, CPU capture/readback and colour conversion, GPU upload, software
decode, no zero-copy, no reference-frame invalidation/FEC. Rate changes recreate FFmpeg contexts,
so they can pause briefly. Baseline installers do not bundle the optional FFmpeg SDK/runtime.
No real Windows/Linux GPU or WAN performance claim follows from the shared backend compiling.
