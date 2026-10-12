//! Windows screen capture with DXGI Desktop Duplication.
//!
//! **Verification status:** type-checked for `x86_64-pc-windows-msvc`; not run here
//! (docs/STATUS.md).
//!
//! * `AcquireNextFrame` blocks until the desktop actually changes, so an idle desktop costs
//!   nothing; per-frame dirty rectangles are forwarded.
//! * The duplication is bound to the desktop the thread is attached to. Every few hundred
//!   milliseconds (and whenever DXGI reports `DXGI_ERROR_ACCESS_LOST`) the capture thread
//!   re-attaches itself to the current input desktop (`nexus-desktop`) and rebuilds the
//!   duplication if the desktop changed, so the picture follows the UAC prompt and the lock
//!   screen. Capturing the *secure* desktop needs a SYSTEM process in the session
//!   (`nexus-winservice`); for a normal user token the attach is refused and capture carries on
//!   with the user's own desktop (which goes black while UAC is up: a Windows rule).
//! * Cursor: position, visibility and shape come from `GetCursorInfo` / `GetIconInfo`
//!   (the duplication's own pointer data is ignored; the cursor is never part of the picture
//!   because Desktop Duplication does not composite it).
//! * Monitor `x`/`y`/`width`/`height` are physical pixels (the process must be per-monitor
//!   DPI aware, which the agent manifest declares); `scale` is 1.0.

use std::time::Duration;

use bytes::Bytes;
use nexus_codec::{Frame, FrameFormat, Rect};
use nexus_core::clock::now_us;
use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
    DXGI_OUTPUT_DESC, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, IDXGIOutput1,
    IDXGIOutputDuplication, IDXGIResource,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits,
    GetObjectW, HGDIOBJ, ReleaseDC,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSORINFO, GetCursorInfo, GetIconInfo, ICONINFO,
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
    MonitorInfo {
        index: u8::try_from(index).unwrap_or(u8::MAX),
        name: String::from_utf16_lossy(&o.desc.DeviceName[..name_len]),
        x: r.left,
        y: r.top,
        width: u32::try_from(r.right - r.left).unwrap_or(0),
        height: u32::try_from(r.bottom - r.top).unwrap_or(0),
        scale: 1.0,
        refresh_hz: 0.0,
        primary: r.left == 0 && r.top == 0,
    }
}

impl CaptureBackend for WindowsBackend {
    fn name(&self) -> &'static str {
        "dxgi-duplication"
    }

    fn monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError> {
        Ok(enumerate()?
            .iter()
            .enumerate()
            .map(|(i, o)| info(i, o))
            .collect())
    }

    fn open(&self, index: u8) -> Result<Box<dyn Capturer>, CaptureError> {
        let outputs = enumerate()?;
        let o = outputs
            .get(usize::from(index))
            .ok_or(CaptureError::NoSuchMonitor(index))?;
        Ok(Box::new(DxgiCapturer::new(info(usize::from(index), o), o)?))
    }

    fn cursor_source(&self) -> Result<Option<Box<dyn CursorSource>>, CaptureError> {
        Ok(Some(Box::new(WinCursor::default())))
    }
}

struct DxgiCapturer {
    monitor: MonitorInfo,
    adapter: IDXGIAdapter1,
    output: IDXGIOutput,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    dup: IDXGIOutputDuplication,
    staging: ID3D11Texture2D,
    first: bool,
    /// Attached lazily on the capture thread (the thread that calls `next_frame`).
    desktop: nexus_desktop::ThreadDesktop,
    last_desktop_check: Option<std::time::Instant>,
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
    fn new(monitor: MonitorInfo, o: &OutputRef) -> Result<Self, CaptureError> {
        let (device, context) = create_device(&o.adapter)?;
        let dup = duplicate(&o.output, &device)?;
        let staging = staging_texture(&device, monitor.width, monitor.height)?;
        Ok(Self {
            monitor,
            adapter: o.adapter.clone(),
            output: o.output.clone(),
            device,
            context,
            dup,
            staging,
            first: true,
            desktop: nexus_desktop::ThreadDesktop::new(),
            last_desktop_check: None,
        })
    }

    fn recreate(&mut self) -> Result<(), CaptureError> {
        // The old duplication is dead after a desktop switch; build a fresh one on the desktop
        // this thread is now attached to.
        let _ = self.desktop.sync();
        self.dup = duplicate(&self.output, &self.device)?;
        let _ = &self.adapter;
        Ok(())
    }

    /// Re-attaches to the input desktop when it changed (at most five checks a second) and
    /// rebuilds the duplication. Returns true if the duplication was rebuilt.
    fn follow_desktop(&mut self) -> Result<bool, CaptureError> {
        let due = self
            .last_desktop_check
            .is_none_or(|t| t.elapsed() >= Duration::from_millis(200));
        if !due {
            return Ok(false);
        }
        self.last_desktop_check = Some(std::time::Instant::now());
        // Refused for a non-SYSTEM process facing the secure desktop: keep what we have.
        match self.desktop.sync() {
            Ok(Some(_)) => {
                self.dup = duplicate(&self.output, &self.device)?;
                self.first = true;
                Ok(true)
            }
            Ok(None) | Err(_) => Ok(false),
        }
    }

    fn dirty_rects(&self) -> Vec<Rect> {
        let mut needed = 0u32;
        // SAFETY: first call only queries the required size.
        let probe = unsafe {
            self.dup
                .GetFrameDirtyRects(0, std::ptr::null_mut(), &mut needed)
        };
        if probe.is_err() && needed == 0 {
            return Vec::new();
        }
        let count = needed as usize / std::mem::size_of::<RECT>();
        let mut rects = vec![RECT::default(); count];
        // SAFETY: the buffer holds `needed` bytes.
        if unsafe {
            self.dup
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
        let timeout_ms = u32::try_from(timeout.as_millis())
            .unwrap_or(u32::MAX)
            .max(1);
        // A new desktop means a new picture; `first` makes the next frame a full one.
        if self.follow_desktop()? {
            return Ok(None);
        }
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        // SAFETY: valid out-pointers; the duplication is live.
        let acquired = unsafe {
            self.dup.AcquireNextFrame(
                if self.first { 1000 } else { timeout_ms },
                &mut info,
                &mut resource,
            )
        };
        match acquired {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                self.recreate()?;
                return Ok(None);
            }
            Err(e) => return Err(failed(e)),
        }
        // A frame that only moved the mouse has no new picture.
        let picture_changed = info.LastPresentTime != 0 || info.AccumulatedFrames > 0;
        let result = (|| -> Result<Option<Frame>, CaptureError> {
            if !picture_changed && !self.first {
                return Ok(None);
            }
            let tex: ID3D11Texture2D = resource
                .as_ref()
                .ok_or_else(|| failed("no desktop resource"))?
                .cast()
                .map_err(failed)?;
            let dirty = if self.first {
                Vec::new()
            } else {
                self.dirty_rects()
            };
            // SAFETY: both textures are live and same-sized; the mapped pointer is only read
            // between Map and Unmap.
            unsafe {
                self.context.CopyResource(&self.staging, &tex);
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                self.context
                    .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                    .map_err(failed)?;
                let (w, h) = (self.monitor.width as usize, self.monitor.height as usize);
                let pitch = mapped.RowPitch as usize;
                let src = mapped.pData.cast::<u8>();
                let mut data = Vec::with_capacity(w * h * 4);
                for row in 0..h {
                    data.extend_from_slice(std::slice::from_raw_parts(src.add(row * pitch), w * 4));
                }
                self.context.Unmap(&self.staging, 0);
                self.first = false;
                Ok(Some(Frame {
                    width: self.monitor.width,
                    height: self.monitor.height,
                    stride: w * 4,
                    format: FrameFormat::Bgra8,
                    data: Bytes::from(data),
                    captured_us: now_us(),
                    dirty,
                }))
            }
        })();
        // SAFETY: a frame was acquired above and must be released exactly once.
        let _ = unsafe { self.dup.ReleaseFrame() };
        result
    }
}

/// Cursor via `GetCursorInfo` + `GetIconInfo`.
#[derive(Default)]
struct WinCursor {
    last_handle: usize,
    shape_id: u32,
}

impl CursorSource for WinCursor {
    fn poll(&mut self) -> Result<CursorSample, CaptureError> {
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
