//! Windows screen capture with DXGI Desktop Duplication.
//!
//! **Verification status:** Windows control revisions are source-only; no builds or tests were
//! run for this change (docs/STATUS.md).
//!
//! * `AcquireNextFrame` blocks until the desktop actually changes, so an idle desktop costs
//!   nothing; per-frame dirty rectangles are forwarded.
//! * The duplication is bound to the desktop the thread is attached to. Every few hundred
//!   milliseconds (and whenever DXGI reports `DXGI_ERROR_ACCESS_LOST`) the capture thread
//!   re-attaches itself to the current input desktop (`nexus-desktop`) and rebuilds the
//!   duplication if the desktop changed, so the picture follows the UAC prompt and the lock
//!   screen. Capturing the *secure* desktop needs a SYSTEM process in the session
//!   (`nexus-winservice`); an ordinary user agent waits without sending new frames until an
//!   accessible input desktop returns.
//! * Cursor: position, visibility and shape come from `GetCursorInfo` / `GetIconInfo`
//!   (the duplication's own pointer data is ignored; the cursor is never part of the picture
//!   because Desktop Duplication does not composite it).
//! * Monitor `x`/`y`/`width`/`height` are physical pixels (the process must be per-monitor
//!   DPI aware, which the agent manifest declares); `scale` reports display DPI metadata.

use std::time::Duration;

use bytes::Bytes;
use nexus_codec::{Frame, FrameFormat, Rect};
use nexus_core::clock::now_us;
use windows::Win32::Foundation::{HMODULE, LPARAM, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_MODE_ROTATION_ROTATE90, DXGI_MODE_ROTATION_ROTATE180, DXGI_MODE_ROTATION_ROTATE270,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, DXGI_OUTPUT_DESC,
    IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC,
    CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, EnumDisplayMonitors, GetDC,
    GetDIBits, GetMonitorInfoW, GetObjectW, HDC, HGDIOBJ, HMONITOR, MONITORINFOEXW, ReleaseDC,
    SRCCOPY, SelectObject,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSORINFO, GetCursorInfo, GetIconInfo, ICONINFO, MONITORINFOF_PRIMARY,
};
use windows::core::Interface;

use crate::CaptureError;
use crate::types::{
    CaptureBackend, Capturer, CursorBitmap, CursorSample, CursorSource, MonitorInfo, limit_rects,
};

fn failed(e: impl std::fmt::Display) -> CaptureError {
    CaptureError::Failed(e.to_string())
}

/// DXGI Desktop Duplication backend.
#[derive(Debug, Default)]
pub struct WindowsBackend;

impl WindowsBackend {
    /// Creates the backend.
    pub fn new() -> Result<Self, CaptureError> {
        nexus_desktop::enable_physical_coordinates().map_err(failed)?;
        Ok(Self)
    }
}

struct OutputRef {
    adapter: IDXGIAdapter1,
    output: IDXGIOutput,
    desc: DXGI_OUTPUT_DESC,
}

fn enumerate() -> Result<Vec<OutputRef>, CaptureError> {
    // SAFETY: plain DXGI enumeration; every returned interface is owned and released by `windows`.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(failed)?;
        let mut out = Vec::new();
        let mut ai = 0;
        while let Ok(adapter) = factory.EnumAdapters1(ai) {
            let mut oi = 0;
            while let Ok(output) = adapter.EnumOutputs(oi) {
                if let Ok(desc) = output.GetDesc()
                    && desc.AttachedToDesktop.as_bool()
                {
                    out.push(OutputRef {
                        adapter: adapter.clone(),
                        output,
                        desc,
                    });
                }
                oi += 1;
            }
            ai += 1;
        }
        // Primary (origin 0,0) first; otherwise left-to-right.
        out.sort_by_key(|o| {
            (
                o.desc.DesktopCoordinates.left != 0 || o.desc.DesktopCoordinates.top != 0,
                o.desc.DesktopCoordinates.left,
            )
        });
        Ok(out)
    }
}

fn info(index: usize, o: &OutputRef) -> MonitorInfo {
    let r = o.desc.DesktopCoordinates;
    let name_len = o
        .desc
        .DeviceName
        .iter()
        .position(|c| *c == 0)
        .unwrap_or(o.desc.DeviceName.len());
    let (mut dpi_x, mut dpi_y) = (96, 96);
    // SAFETY: the monitor handle comes from DXGI and the output pointers are valid.
    let _ = unsafe { GetDpiForMonitor(o.desc.Monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
    MonitorInfo {
        index: u8::try_from(index).unwrap_or(u8::MAX),
        name: String::from_utf16_lossy(&o.desc.DeviceName[..name_len]),
        x: r.left,
        y: r.top,
        width: u32::try_from(r.right - r.left).unwrap_or(0),
        height: u32::try_from(r.bottom - r.top).unwrap_or(0),
        // Input coordinates remain physical pixels (MonitorRect uses scale=1 on Windows).
        // This value describes the display's logical/UI scale for the controller.
        scale: dpi_x as f32 / 96.0,
        refresh_hz: 0.0,
        primary: r.left == 0 && r.top == 0,
    }
}

fn gdi_monitors() -> Result<Vec<MonitorInfo>, CaptureError> {
    let mut monitors: Vec<MonitorInfo> = Vec::new();
    // SAFETY: synchronous enumeration; the Vec outlives every callback and is never aliased.
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(gdi_monitor),
            LPARAM(std::ptr::from_mut(&mut monitors) as isize),
        )
    };
    if !ok.as_bool() {
        return Err(failed("EnumDisplayMonitors"));
    }
    monitors.sort_by_key(|m| (!m.primary, m.x, m.y));
    for (index, monitor) in monitors.iter_mut().enumerate() {
        monitor.index = u8::try_from(index).unwrap_or(u8::MAX);
    }
    Ok(monitors)
}
unsafe extern "system" fn gdi_monitor(
    handle: HMONITOR,
    _dc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> windows::core::BOOL {
    let mut native = MONITORINFOEXW::default();
    let Ok(size) = u32::try_from(std::mem::size_of::<MONITORINFOEXW>()) else {
        return false.into();
    };
    native.monitorInfo.cbSize = size;
    // SAFETY: the enumeration supplies a live monitor and the structure includes the EX size.
    if !unsafe { GetMonitorInfoW(handle, &mut native.monitorInfo) }.as_bool() {
        return true.into();
    }
    let rect = native.monitorInfo.rcMonitor;
    let (mut dpi_x, mut dpi_y) = (96, 96);
    // SAFETY: live monitor handle and valid writable outputs.
    let _ = unsafe { GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
    let length = native
        .szDevice
        .iter()
        .position(|c| *c == 0)
        .unwrap_or(native.szDevice.len());
    // SAFETY: gdi_monitors passes its exclusively borrowed Vec through LPARAM synchronously.
    let monitors = unsafe { &mut *(data.0 as *mut Vec<MonitorInfo>) };
    monitors.push(MonitorInfo {
        index: 0,
        name: String::from_utf16_lossy(&native.szDevice[..length]),
        x: rect.left,
        y: rect.top,
        width: (rect.right - rect.left).max(0).cast_unsigned(),
        height: (rect.bottom - rect.top).max(0).cast_unsigned(),
        scale: dpi_x as f32 / 96.0,
        refresh_hz: 0.0,
        primary: native.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
    });
    true.into()
}

impl CaptureBackend for WindowsBackend {
    fn name(&self) -> &'static str {
        "dxgi-duplication"
    }

    fn monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError> {
        if let Ok(outputs) = enumerate()
            && !outputs.is_empty()
        {
            return Ok(outputs
                .iter()
                .enumerate()
                .map(|(i, o)| info(i, o))
                .collect());
        }
        gdi_monitors()
    }

    fn open(&self, index: u8) -> Result<Box<dyn Capturer>, CaptureError> {
        let monitor = self
            .monitors()?
            .into_iter()
            .find(|m| m.index == index)
            .ok_or(CaptureError::NoSuchMonitor(index))?;
        Ok(Box::new(DxgiCapturer::new(monitor)))
    }

    fn cursor_source(&self) -> Result<Option<Box<dyn CursorSource>>, CaptureError> {
        Ok(Some(Box::new(WinCursor::default())))
    }
}

struct DxgiCapturer {
    monitor: MonitorInfo,
    gpu: Option<DxgiResources>,
    first: bool,
    desktop: nexus_desktop::ThreadDesktop,
    retry_after: Option<std::time::Instant>,
}

struct DxgiResources {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    dup: IDXGIOutputDuplication,
    staging: Option<ID3D11Texture2D>,
    staging_size: (u32, u32),
    rotation: windows::Win32::Graphics::Dxgi::Common::DXGI_MODE_ROTATION,
}

// SAFETY: the D3D11/DXGI objects are used from the single capture thread that owns this value;
// the immediate context is not shared.
unsafe impl Send for DxgiCapturer {}

fn create_device(
    adapter: &IDXGIAdapter1,
) -> Result<(ID3D11Device, ID3D11DeviceContext), CaptureError> {
    let mut device = None;
    let mut context = None;
    // SAFETY: valid out-pointers; the adapter is live.
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
        .map_err(failed)?;
    }
    Ok((
        device.ok_or_else(|| failed("no D3D11 device"))?,
        context.ok_or_else(|| failed("no D3D11 context"))?,
    ))
}

fn duplicate(
    output: &IDXGIOutput,
    device: &ID3D11Device,
) -> Result<IDXGIOutputDuplication, CaptureError> {
    let output1: IDXGIOutput1 = output.cast().map_err(failed)?;
    // SAFETY: both interfaces are live.
    unsafe { output1.DuplicateOutput(device) }.map_err(|e| {
        // E_ACCESSDENIED while another process (or the secure desktop) owns the duplication.
        CaptureError::Failed(format!(
            "DuplicateOutput: {e} (is the secure desktop active, or is the session disconnected?)"
        ))
    })
}

fn staging_texture(device: &ID3D11Device, w: u32, h: u32) -> Result<ID3D11Texture2D, CaptureError> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0.cast_unsigned(),
        MiscFlags: 0,
    };
    let mut tex = None;
    // SAFETY: valid descriptor and out-pointer.
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }.map_err(failed)?;
    tex.ok_or_else(|| failed("no staging texture"))
}

impl DxgiCapturer {
    fn new(monitor: MonitorInfo) -> Self {
        // Desktop-bound GPU objects MUST be created on the dedicated capture thread, after
        // attaching it. Opening a stream while Winlogon is active must not duplicate Default.
        Self {
            monitor,
            gpu: None,
            first: true,
            desktop: nexus_desktop::ThreadDesktop::new(),
            retry_after: None,
        }
    }

    fn recreate(&mut self) -> Result<(), CaptureError> {
        self.gpu = None;
        let outputs = enumerate()?;
        // Stream indices address the latest announced monitor list; preserve the index across
        // hot-plug so video and absolute input continue referring to the same current output.
        let output = outputs
            .get(usize::from(self.monitor.index))
            .ok_or(CaptureError::NoSuchMonitor(self.monitor.index))?;
        self.monitor = info(usize::from(self.monitor.index), output);
        let (device, context) = create_device(&output.adapter)?;
        let dup = duplicate(&output.output, &device)?;
        self.gpu = Some(DxgiResources {
            device,
            context,
            dup,
            staging: None,
            staging_size: (0, 0),
            rotation: output.desc.Rotation,
        });
        self.first = true;
        self.retry_after = None;
        Ok(())
    }

    fn follow_desktop(&mut self) -> Result<(), CaptureError> {
        let input = nexus_desktop::input_desktop_name().map_err(failed)?;
        if self.desktop.attached().is_none_or(|name| name != input) {
            // Release duplication and D3D state before attaching to the new desktop.
            self.gpu = None;
            self.retry_after = None;
            self.desktop.sync().map_err(failed)?;
            self.first = true;
        }
        Ok(())
    }

    fn dirty_rects(&self) -> Vec<Rect> {
        let Some(gpu) = &self.gpu else {
            return Vec::new();
        };
        let mut moved_bytes = 0u32;
        // SAFETY: zero-sized query returns the byte count without writing rectangle data.
        let _ = unsafe {
            gpu.dup
                .GetFrameMoveRects(0, std::ptr::null_mut(), &mut moved_bytes)
        };
        if moved_bytes != 0 {
            return Vec::new();
        }
        let mut needed = 0u32;
        // SAFETY: first call only queries the required size.
        let probe = unsafe {
            gpu.dup
                .GetFrameDirtyRects(0, std::ptr::null_mut(), &mut needed)
        };
        if probe.is_err() && needed == 0 {
            return Vec::new();
        }
        let count = needed as usize / std::mem::size_of::<RECT>();
        let mut rects = vec![RECT::default(); count];
        // SAFETY: the buffer holds `needed` bytes.
        if unsafe {
            gpu.dup
                .GetFrameDirtyRects(needed, rects.as_mut_ptr(), &mut needed)
        }
        .is_err()
        {
            return Vec::new();
        }
        let clip = |v: i32, max: u32| {
            u16::try_from(v.clamp(0, i32::try_from(max).unwrap_or(i32::MAX))).unwrap_or(u16::MAX)
        };
        let out: Vec<Rect> = rects
            .iter()
            .filter(|r| r.right > r.left && r.bottom > r.top)
            .map(|r| Rect {
                x: clip(r.left, self.monitor.width),
                y: clip(r.top, self.monitor.height),
                w: clip(r.right - r.left, self.monitor.width),
                h: clip(r.bottom - r.top, self.monitor.height),
            })
            .collect();
        limit_rects(out, 64)
    }
}

impl Capturer for DxgiCapturer {
    fn name(&self) -> &'static str {
        "dxgi-duplication"
    }

    fn monitor(&self) -> &MonitorInfo {
        &self.monitor
    }

    fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>, CaptureError> {
        // Desktop transitions are expected, including the brief no-input-desktop interval.
        // Return no frame and retry instead of permanently ending a remote video pipeline.
        if let Err(e) = self.follow_desktop() {
            tracing::debug!(error = %e, "waiting for an accessible Windows desktop");
            std::thread::sleep(timeout.max(Duration::from_millis(16)));
            return Ok(None);
        }
        if self.gpu.is_none()
            && self
                .retry_after
                .is_none_or(|t| std::time::Instant::now() >= t)
            && let Err(e) = self.recreate()
        {
            tracing::debug!(error = %e, "DXGI unavailable; using desktop GDI capture");
            self.retry_after = Some(std::time::Instant::now() + Duration::from_secs(1));
        }
        if self.gpu.is_none() {
            // DXGI may be absent on RDP virtual displays or during secure-desktop transitions.
            // This runs on the already authenticated/attached capture thread, never Session 0.
            std::thread::sleep(timeout.max(Duration::from_millis(16)));
            if let Ok(monitors) = gdi_monitors()
                && let Some(monitor) = monitors.into_iter().find(|m| m.index == self.monitor.index)
            {
                self.monitor = monitor;
            }
            return match gdi_frame(&self.monitor) {
                Ok(frame) => Ok(Some(frame)),
                Err(e) => {
                    tracing::debug!(error = %e, "waiting for Windows display resources");
                    Ok(None)
                }
            };
        }
        let timeout_ms = u32::try_from(timeout.as_millis())
            .unwrap_or(u32::MAX)
            .max(1);
        let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        let gpu = self
            .gpu
            .as_ref()
            .ok_or_else(|| failed("no DXGI resources"))?;
        // SAFETY: valid output pointers and live duplication owned by this capture thread.
        let acquired = unsafe {
            gpu.dup
                .AcquireNextFrame(timeout_ms, &mut frame_info, &mut resource)
        };
        match acquired {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
            Err(e) => {
                // Access lost, session disconnected and device reset all require reconstruction.
                tracing::debug!(error = %e, "rebuilding Windows capture after display transition");
                self.gpu = None;
                self.first = true;
                std::thread::sleep(Duration::from_millis(16));
                return Ok(None);
            }
        }
        let initial_frame = self.first;
        let result = (|| -> Result<Option<Frame>, CaptureError> {
            if frame_info.LastPresentTime == 0 && frame_info.AccumulatedFrames == 0 && !self.first {
                return Ok(None);
            }
            let tex: ID3D11Texture2D = resource
                .as_ref()
                .ok_or_else(|| failed("no desktop resource"))?
                .cast()
                .map_err(failed)?;
            // Move rectangles can relocate unchanged pixels. Send full damage rather than omit
            // those destinations; rotated monitors also need damage in display coordinates.
            let gpu = self
                .gpu
                .as_ref()
                .ok_or_else(|| failed("no DXGI resources"))?;
            let dirty = if self.first
                || gpu.rotation
                    != windows::Win32::Graphics::Dxgi::Common::DXGI_MODE_ROTATION_IDENTITY
            {
                Vec::new()
            } else {
                self.dirty_rects()
            };
            let gpu = self
                .gpu
                .as_mut()
                .ok_or_else(|| failed("no DXGI resources"))?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            // SAFETY: live texture and writable descriptor.
            unsafe { tex.GetDesc(&mut desc) };
            if gpu.staging_size != (desc.Width, desc.Height) {
                gpu.staging = Some(staging_texture(&gpu.device, desc.Width, desc.Height)?);
                gpu.staging_size = (desc.Width, desc.Height);
            }
            let staging = gpu
                .staging
                .as_ref()
                .ok_or_else(|| failed("no staging texture"))?;
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            // SAFETY: same-sized BGRA textures; mapped memory is only read before Unmap.
            unsafe {
                gpu.context.CopyResource(staging, &tex);
                gpu.context
                    .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                    .map_err(failed)?;
            }
            let (source_w, source_h) = (desc.Width as usize, desc.Height as usize);
            let rotated = gpu.rotation == DXGI_MODE_ROTATION_ROTATE90
                || gpu.rotation == DXGI_MODE_ROTATION_ROTATE270;
            let (w, h) = if rotated {
                (source_h, source_w)
            } else {
                (source_w, source_h)
            };
            let (frame_width, frame_height) = if rotated {
                (desc.Height, desc.Width)
            } else {
                (desc.Width, desc.Height)
            };
            let mut data = vec![0u8; w * h * 4];
            let pitch = mapped.RowPitch as usize;
            let source = mapped.pData.cast::<u8>();
            for y in 0..h {
                if !rotated && gpu.rotation != DXGI_MODE_ROTATION_ROTATE180 {
                    // SAFETY: full row lies within the mapped BGRA texture allocation.
                    let row = unsafe { std::slice::from_raw_parts(source.add(y * pitch), w * 4) };
                    data[y * w * 4..(y + 1) * w * 4].copy_from_slice(row);
                    continue;
                }
                for x in 0..w {
                    let (sx, sy) = match gpu.rotation {
                        DXGI_MODE_ROTATION_ROTATE90 => (y, source_h - 1 - x),
                        DXGI_MODE_ROTATION_ROTATE180 => (source_w - 1 - x, source_h - 1 - y),
                        DXGI_MODE_ROTATION_ROTATE270 => (source_w - 1 - y, x),
                        _ => (x, y),
                    };
                    // SAFETY: rotation maps each output pixel to an in-bounds source texel;
                    // RowPitch describes the mapped allocation, which is still mapped here.
                    let pixel =
                        unsafe { std::slice::from_raw_parts(source.add(sy * pitch + sx * 4), 4) };
                    let offset = (y * w + x) * 4;
                    data[offset..offset + 4].copy_from_slice(pixel);
                }
            }
            // SAFETY: balances the successful Map, after all mapped memory reads.
            unsafe { gpu.context.Unmap(staging, 0) };
            self.first = false;
            self.monitor.width = frame_width;
            self.monitor.height = frame_height;
            Ok(Some(Frame {
                width: frame_width,
                height: frame_height,
                stride: w * 4,
                format: FrameFormat::Bgra8,
                data: Bytes::from(data),
                captured_us: now_us(),
                dirty,
            }))
        })();
        if let Some(gpu) = &self.gpu {
            // SAFETY: exactly one ReleaseFrame for the acquisition above, including error paths.
            let _ = unsafe { gpu.dup.ReleaseFrame() };
        }
        if let Err(e) = &result {
            tracing::debug!(error = %e, "rebuilding Windows capture after frame read failure");
            self.gpu = None;
            self.first = true;
            return Ok(None);
        }
        // Some Windows drivers supply an empty initial duplication surface even on a live
        // desktop. Confirm that GDI sees actual pixels before choosing the existing fallback;
        // preserve intentionally black screens and DXGI's protected-content masking.
        if initial_frame
            && !frame_info.ProtectedContentMaskedOut.as_bool()
            && let Ok(Some(frame)) = &result
            && frame
                .data
                .chunks_exact(4)
                .all(|pixel| pixel[..3] == [0, 0, 0])
            && let Ok(fallback) = gdi_frame(&self.monitor)
            && fallback
                .data
                .chunks_exact(4)
                .any(|pixel| pixel[..3] != [0, 0, 0])
        {
            tracing::debug!("DXGI returned an empty initial surface; using GDI capture");
            self.gpu = None;
            self.first = true;
            self.retry_after = Some(std::time::Instant::now() + Duration::from_secs(1));
            return Ok(Some(fallback));
        }
        result
    }
}

/// CPU fallback for Winlogon and RDP virtual displays when desktop duplication is unavailable.
fn gdi_frame(monitor: &MonitorInfo) -> Result<Frame, CaptureError> {
    let width = i32::try_from(monitor.width).map_err(failed)?;
    let height = i32::try_from(monitor.height).map_err(failed)?;
    if width <= 0 || height <= 0 {
        return Err(failed("empty Windows display"));
    }
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: u32::try_from(std::mem::size_of::<BITMAPINFOHEADER>()).map_err(failed)?,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: GDI objects belong to this capture thread and each is released on every path.
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return Err(failed("GetDC(input desktop)"));
        }
        let memory = CreateCompatibleDC(Some(screen));
        if memory.is_invalid() {
            let _ = ReleaseDC(None, screen);
            return Err(failed("CreateCompatibleDC"));
        }
        let mut pixels = std::ptr::null_mut();
        let bitmap =
            match CreateDIBSection(Some(screen), &bmi, DIB_RGB_COLORS, &mut pixels, None, 0) {
                Ok(bitmap) => bitmap,
                Err(e) => {
                    let _ = DeleteDC(memory);
                    let _ = ReleaseDC(None, screen);
                    return Err(failed(e));
                }
            };
        let old = SelectObject(memory, HGDIOBJ(bitmap.0));
        let result = BitBlt(
            memory,
            0,
            0,
            width,
            height,
            Some(screen),
            monitor.x,
            monitor.y,
            SRCCOPY | CAPTUREBLT,
        )
        .map_err(failed)
        .and_then(|()| {
            if pixels.is_null() {
                return Err(failed("GDI returned no pixels"));
            }
            // The top-down 32-bpp DIB has width*4 bytes per row, no extra row padding.
            let data = std::slice::from_raw_parts(
                pixels.cast::<u8>(),
                monitor.width as usize * monitor.height as usize * 4,
            )
            .to_vec();
            Ok(Frame {
                width: monitor.width,
                height: monitor.height,
                stride: monitor.width as usize * 4,
                format: FrameFormat::Bgra8,
                data: Bytes::from(data),
                captured_us: now_us(),
                dirty: Vec::new(),
            })
        });
        let _ = SelectObject(memory, old);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(memory);
        let _ = ReleaseDC(None, screen);
        result
    }
}

/// Cursor via `GetCursorInfo` + `GetIconInfo`.
#[derive(Default)]
struct WinCursor {
    last_handle: usize,
    shape_id: u32,
    desktop: nexus_desktop::ThreadDesktop,
}

impl CursorSource for WinCursor {
    fn poll(&mut self) -> Result<CursorSample, CaptureError> {
        self.desktop.sync().map_err(failed)?;
        let mut ci = CURSORINFO {
            cbSize: u32::try_from(std::mem::size_of::<CURSORINFO>()).unwrap_or(0),
            ..Default::default()
        };
        // SAFETY: `ci.cbSize` is set as the API requires.
        unsafe { GetCursorInfo(&mut ci) }.map_err(failed)?;
        let handle = ci.hCursor.0 as usize;
        let visible = ci.flags.0 & CURSOR_SHOWING.0 != 0;
        let mut new_shape = None;
        if handle != self.last_handle && handle != 0 {
            self.last_handle = handle;
            self.shape_id = self.shape_id.wrapping_add(1);
            new_shape = cursor_bitmap(&ci, self.shape_id);
        }
        Ok(CursorSample {
            x: ci.ptScreenPos.x,
            y: ci.ptScreenPos.y,
            visible,
            shape_id: self.shape_id,
            new_shape,
        })
    }
}

/// Renders the current cursor into premultiplied BGRA.
fn cursor_bitmap(ci: &CURSORINFO, shape_id: u32) -> Option<CursorBitmap> {
    // SAFETY: GDI calls on handles obtained from the system in this function; every created
    // bitmap is deleted before returning.
    unsafe {
        let mut icon = ICONINFO::default();
        GetIconInfo(
            windows::Win32::UI::WindowsAndMessaging::HICON(ci.hCursor.0),
            &mut icon,
        )
        .ok()?;
        let color = icon.hbmColor;
        let mask = icon.hbmMask;
        let result = (|| {
            let probe = if color.is_invalid() { mask } else { color };
            let mut bm = BITMAP::default();
            if GetObjectW(
                HGDIOBJ(probe.0),
                i32::try_from(std::mem::size_of::<BITMAP>()).ok()?,
                Some((&raw mut bm).cast()),
            ) == 0
            {
                return None;
            }
            let w = u32::try_from(bm.bmWidth).ok()?;
            let full_h = u32::try_from(bm.bmHeight).ok()?;
            // Monochrome cursors stack the AND mask over the XOR mask in one bitmap.
            let h = if color.is_invalid() {
                full_h / 2
            } else {
                full_h
            };
            if w == 0 || h == 0 || w > 256 || h > 256 {
                return None;
            }
            let hdc = GetDC(None);
            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: u32::try_from(std::mem::size_of::<BITMAPINFOHEADER>()).ok()?,
                    biWidth: i32::try_from(w).ok()?,
                    biHeight: -i32::try_from(full_h).ok()?, // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut px = vec![0u8; (w * full_h * 4) as usize];
            let got = GetDIBits(
                hdc,
                probe,
                0,
                full_h,
                Some(px.as_mut_ptr().cast()),
                &mut bmi,
                DIB_RGB_COLORS,
            );
            ReleaseDC(None, hdc);
            if got == 0 {
                return None;
            }
            let bgra: Vec<u8> = if color.is_invalid() {
                // AND mask on top, XOR below: AND=1,XOR=0 transparent; AND=0 → black/white by XOR.
                let (and, xor) = px.split_at((w * h * 4) as usize);
                and.chunks_exact(4)
                    .zip(xor.chunks_exact(4))
                    .flat_map(|(a, x)| {
                        let transparent = a[0] != 0 && x[0] == 0;
                        let v = if x[0] != 0 { 255 } else { 0 };
                        if transparent {
                            [0, 0, 0, 0]
                        } else {
                            [v, v, v, 255]
                        }
                    })
                    .collect()
            } else {
                // Colour cursors normally carry alpha; if every alpha is zero, treat as opaque.
                let has_alpha = px.chunks_exact(4).any(|p| p[3] != 0);
                px.chunks_exact_mut(4).for_each(|p| {
                    if !has_alpha {
                        p[3] = 255;
                    } else {
                        // GDI gives straight alpha; the wire format is premultiplied.
                        let a = u16::from(p[3]);
                        for c in &mut p[..3] {
                            *c = u8::try_from(u16::from(*c) * a / 255).unwrap_or(255);
                        }
                    }
                });
                px
            };
            Some(CursorBitmap {
                shape_id,
                width: w,
                height: h,
                hot_x: icon.xHotspot,
                hot_y: icon.yHotspot,
                bgra: Bytes::from(bgra),
            })
        })();
        if !color.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(color.0));
        }
        if !mask.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(mask.0));
        }
        result
    }
}
