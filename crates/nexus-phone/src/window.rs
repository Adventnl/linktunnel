//! Narrow Win32 boundary for the scrcpy process we own. No window is selected by title.
use windows_sys::Win32::{Foundation::*, UI::WindowsAndMessaging::*};

/// Physical client coordinates supplied by the desktop shell.
#[derive(Clone, Copy)]
pub struct Bounds {
    /// Left within the dashboard client area.
    pub x: i32,
    /// Top within the dashboard client area.
    pub y: i32,
    /// Requested client width.
    pub width: u32,
    /// Requested client height.
    pub height: u32,
    /// Hide the picture while a dialog or another device is selected.
    pub visible: bool,
    /// Present the same viewer in its own decorated window.
    pub detached: bool,
}

pub(crate) struct Window {
    handle: isize,
    pid: u32,
    pub width: u32,
    pub height: u32,
}

struct Search {
    pid: u32,
    found: Option<Window>,
}

unsafe extern "system" fn visit(hwnd: HWND, data: LPARAM) -> i32 {
    // SAFETY: EnumWindows calls synchronously while the stack-owned Search is live.
    let search = unsafe { &mut *(data as *mut Search) };
    let mut pid = 0;
    let mut class = [0u16; 128];
    let mut rect = RECT::default();
    // SAFETY: Windows owns hwnd; all output buffers are valid for these synchronous calls.
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut pid);
        let len = GetClassNameW(hwnd, class.as_mut_ptr(), 128);
        if pid == search.pid
            && IsWindowVisible(hwnd) != 0
            && len > 0
            && String::from_utf16_lossy(&class[..usize::try_from(len).unwrap_or_default()])
                .starts_with("SDL")
            && GetClientRect(hwnd, &mut rect) != 0
            && rect.right > 0
            && rect.bottom > 0
        {
            search.found = Some(Window {
                handle: hwnd as isize,
                pid,
                width: u32::try_from(rect.right).unwrap_or_default(),
                height: u32::try_from(rect.bottom).unwrap_or_default(),
            });
            return 0;
        }
    }
    1
}

pub(crate) fn visible(pid: u32) -> Option<Window> {
    let mut search = Search { pid, found: None };
    // SAFETY: the callback borrows search only for this synchronous enumeration.
    unsafe {
        EnumWindows(Some(visit), &mut search as *mut Search as isize);
    }
    search.found
}

impl Window {
    pub fn layout(&self, parent: isize, bounds: Bounds) -> Result<(), String> {
        if bounds.x < 0
            || bounds.y < 0
            || bounds.x > 32768
            || bounds.y > 32768
            || !(1..=32768).contains(&bounds.width)
            || !(1..=32768).contains(&bounds.height)
        {
            return Err("Invalid phone viewport.".into());
        }
        let hwnd = self.handle as HWND;
        let parent = parent as HWND;
        let mut pid = 0;
        // SAFETY: validate OS handles and process ownership before changing any window.
        // No borrowed memory survives these calls. The Phone owns the child lifetime.
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid != self.pid || IsWindow(hwnd) == 0 {
                return Ok(());
            }
            if IsWindow(parent) == 0 {
                return Err("Phone workspace window is unavailable.".into());
            }
            let target = if bounds.detached {
                std::ptr::null_mut()
            } else {
                parent
            };
            if GetParent(hwnd) != target {
                ShowWindow(hwnd, SW_HIDE);
                let old = GetWindowLongW(hwnd, GWL_STYLE);
                let old_bits = u32::from_ne_bytes(old.to_ne_bytes());
                let style = if bounds.detached {
                    (old_bits & !WS_CHILD) | WS_OVERLAPPEDWINDOW
                } else {
                    (old_bits & !(WS_POPUP | WS_OVERLAPPEDWINDOW)) | WS_CHILD | WS_CLIPSIBLINGS
                };
                SetWindowLongW(hwnd, GWL_STYLE, i32::from_ne_bytes(style.to_ne_bytes()));
                SetParent(hwnd, target);
                if GetParent(hwnd) != target {
                    SetWindowLongW(hwnd, GWL_STYLE, old);
                    ShowWindow(hwnd, SW_SHOW);
                    return Err("Windows could not place the phone in the workspace.".into());
                }
                if bounds.detached {
                    let width = self.width.min(720);
                    let height = self.height.min(850);
                    let (width, height) = if u64::from(width) * u64::from(self.height)
                        > u64::from(height) * u64::from(self.width)
                    {
                        (
                            u64::from(self.width) * u64::from(height) / u64::from(self.height),
                            u64::from(height),
                        )
                    } else {
                        (
                            u64::from(width),
                            u64::from(self.height) * u64::from(width) / u64::from(self.width),
                        )
                    };
                    let mut rect = RECT {
                        left: 0,
                        top: 0,
                        right: i32::try_from(width).unwrap_or(720),
                        bottom: i32::try_from(height).unwrap_or(850),
                    };
                    AdjustWindowRectEx(
                        &mut rect,
                        style,
                        0,
                        u32::from_ne_bytes(GetWindowLongW(hwnd, GWL_EXSTYLE).to_ne_bytes()),
                    );
                    SetWindowPos(
                        hwnd,
                        std::ptr::null_mut(),
                        100,
                        100,
                        rect.right - rect.left,
                        rect.bottom - rect.top,
                        SWP_FRAMECHANGED | SWP_NOACTIVATE,
                    );
                }
            }
            if !bounds.detached {
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    bounds.x,
                    bounds.y,
                    i32::try_from(bounds.width).map_err(|_| "Invalid phone width")?,
                    i32::try_from(bounds.height).map_err(|_| "Invalid phone height")?,
                    SWP_FRAMECHANGED | SWP_NOACTIVATE,
                );
            }
            ShowWindow(
                hwnd,
                if bounds.visible || bounds.detached {
                    SW_SHOWNA
                } else {
                    SW_HIDE
                },
            );
        }
        Ok(())
    }
}
