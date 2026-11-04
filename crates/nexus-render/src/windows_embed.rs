//! Windows child surface boundary.
//!
//! Safety contract: the launcher supplies its own live HWND and owns the viewer lifetime.
//! Validate the handle before passing it to winit; Windows destroys child HWNDs with the
//! parent. No borrowed Rust memory crosses the boundary.
use raw_window_handle::{RawWindowHandle, Win32WindowHandle};
use std::num::NonZeroIsize;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::IsWindow;
use winit::window::WindowAttributes;

pub(crate) fn attributes(
    attrs: WindowAttributes,
    parent: isize,
) -> Result<WindowAttributes, String> {
    let parent = NonZeroIsize::new(parent).ok_or("invalid viewer parent")?;
    // SAFETY: IsWindow only queries an OS-managed handle, and dereferences no caller memory.
    if !unsafe { IsWindow(Some(HWND(parent.get() as *mut _))) }.as_bool() {
        return Err("viewer parent window is no longer available".into());
    }
    let raw = RawWindowHandle::Win32(Win32WindowHandle::new(parent));
    // SAFETY: validated HWND belongs to the supervising dashboard, which remains alive for
    // this process. Only winit uses the handle; no pointer or reference is fabricated.
    Ok(unsafe { attrs.with_parent_window(Some(raw)) }
        .with_decorations(false)
        .with_visible(false))
}

/// Reparent the existing viewer HWND without replacing its GPU surface or remote session.
pub(crate) fn set_detached(
    window: &winit::window::Window,
    parent: isize,
    detached: bool,
) -> Result<(), String> {
    use raw_window_handle::HasWindowHandle as _;
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_STYLE, GetParent, GetWindowLongPtrW, SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE,
        SWP_NOZORDER, SetParent, SetWindowLongPtrW, SetWindowPos, WS_CHILD, WS_OVERLAPPEDWINDOW,
        WS_POPUP,
    };
    let RawWindowHandle::Win32(handle) =
        window.window_handle().map_err(|e| e.to_string())?.as_raw()
    else {
        return Err("viewer is not a Win32 window".into());
    };
    let hwnd = HWND(handle.hwnd.get() as *mut _);
    let parent = HWND(parent as *mut _);
    // SAFETY: both handles refer to OS-managed windows. The supervisor owns the parent and
    // this process owns the child; no Rust references or caller-owned buffers cross the FFI.
    unsafe {
        if !IsWindow(Some(parent)).as_bool() {
            return Err("workspace window is unavailable".into());
        }
        window.set_fullscreen(None);
        let old_style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        let style = if detached {
            (old_style & !(WS_CHILD.0 as isize)) | WS_OVERLAPPEDWINDOW.0 as isize
        } else {
            (old_style & !((WS_POPUP.0 | WS_OVERLAPPEDWINDOW.0) as isize)) | WS_CHILD.0 as isize
        };
        if detached {
            SetWindowLongPtrW(hwnd, GWL_STYLE, style);
        }
        let target = if detached { None } else { Some(parent) };
        let _ = SetParent(hwnd, target);
        let actual = GetParent(hwnd).ok();
        if actual != target {
            SetWindowLongPtrW(hwnd, GWL_STYLE, old_style);
            return Err("Windows could not move the viewer between windows".into());
        }
        if !detached {
            SetWindowLongPtrW(hwnd, GWL_STYLE, style);
        }
        SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER,
        )
        .map_err(|e| e.to_string())?;
    }
    window.set_visible(true);
    if detached {
        window.focus_window();
    }
    Ok(())
}
