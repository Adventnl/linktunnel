# ADR-0009: Media pipeline, codecs, licensing

**Status:** Accepted (design; implementation phased)

## Decision
Capture → (damage tracking) → GPU-resident encode → per-frame stream → HW decode → GPU texture
→ `wgpu` present. Backends sit behind traits; each is selected at runtime from advertised
capabilities (`Hello`).

| | Capture | Encode | Decode |
|---|---|---|---|
| macOS | ScreenCaptureKit | **VideoToolbox** (H.264/HEVC) | VideoToolbox |
| Windows | Windows.Graphics.Capture, DXGI duplication fallback | NVENC / AMF / QuickSync (via MFT or vendor SDK) | D3D11VA |
| Linux | PipeWire portal (Wayland), XShm (X11), KMS/DRM (headless) | VAAPI, NVENC | VAAPI |
| Software fallback | n/a | **OpenH264** (screen-content real-time mode) | OpenH264 |

* **Licensing:** x264 is GPL; shipping it would force the whole distribution to GPL terms, so
  the software fallback is OpenH264 (BSD) and the preferred paths use the platform's own
  codecs. H.264 patent licensing for software encoders in a *distributed* product is a legal
  matter for the owner, recorded in docs/LIMITATIONS.md; this tool targets personal use.
* **Screen content:** low-latency settings everywhere (no B-frames, short GOP, intra-refresh,
  tight VBV), screen-content mode/tools where the encoder has them.
* **Zero-copy:** frames stay on the GPU (D3D11 texture, IOSurface, DMA-BUF) from capture
  through encode; CPU readback exists only in the software path.
* **Honest scope:** hardware encoders other than VideoToolbox can only be validated on matching
  hardware; they are implemented against the trait and marked *unverified* in STATUS.md until
  run on that hardware.
