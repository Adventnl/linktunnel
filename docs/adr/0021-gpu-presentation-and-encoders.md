# ADR 0021: GPU presentation and optional FFmpeg hardware encode

Accepted 2026-10-12. Extends ADRs 0009 and 0012.

Keep the existing native winit viewer and its input mappings; present with wgpu 30 on Metal,
D3D12 or Vulkan. Upload the decoded BGRA image once, scale in the shader, preserve the aspect
ratio and nearest-neighbour pixel rendering, clear letterboxes, request AutoNoVsync and one
frame of swapchain latency. Actual present mode is platform dependent. Keep softbuffer when
initialization cannot find a GPU or NEXUS_SOFTWARE_VIEWER is set.

Add the optional hardware-ffmpeg codec/agent feature. Use FFmpeg's NVENC, AMF, Quick Sync and
VAAPI H.264 encoders instead of maintaining four vendor SDK ABIs. Probe actual immediate Annex-B
IDR output with SPS/PPS; compiled codec names alone do not imply working hardware. No B-frames
or lookahead; configure vendor low-delay options. VAAPI uploads NV12 through a documented,
isolated FFI module. Missing devices/codecs fail selection and retain OpenH264 fallback.
VideoToolbox remains macOS's preferred backend and needs no FFmpeg installation.

Rate changes reopen a vendor context transactionally and force the next keyframe because
vendor runtime reconfiguration is inconsistent. Reopening is correct but can cause a brief
pause; measure before replacing it with vendor-specific runtime APIs. Current capture and decode
remain CPU buffers; these changes do not establish zero-copy or hardware decoding. FFmpeg is
optional to avoid changing portable baseline build/runtime prerequisites. Matching GPU lab
validation and FFmpeg redistribution decisions are still required for installer acceleration.
