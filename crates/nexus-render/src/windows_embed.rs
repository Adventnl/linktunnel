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
