//! Per-session visible badge and physical panic shortcut, independent of the Tauri dashboard.
//! Windows/hooks stay on their own desktop-following thread so input/capture can switch freely.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};
static PANIC: AtomicBool = AtomicBool::new(false);

/// Owns the target's indicator/panic thread and stops it before the agent is dropped.
pub struct SessionUi {
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl SessionUi {
    /// `state` returns (has active incoming sessions, agent is stopping); `panic` disables
    /// access. `label` is localized by the agent. The physical hotkey works on Winlogon too.
    pub fn start(
        label: String,
        state: impl Fn() -> (bool, bool) + Send + 'static,
        panic: impl Fn() + Send + 'static,
    ) -> Result<Self, std::io::Error> {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = std::thread::Builder::new()
            .name("session-indicator".into())
            .spawn(move || {
                if let Err(e) = run(&label, &worker_stop, state, panic) {
                    tracing::warn!(error = %e, "Windows session indicator unavailable");
                }
            })?;
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
}
impl Drop for SessionUi {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
#[derive(Default)]
struct Resources {
    window: Option<HWND>,
    hook: Option<HHOOK>,
}
impl Drop for Resources {
    fn drop(&mut self) {
        // SAFETY: owned handles created on this UI thread, removed before changing desktops.
        unsafe {
            if let Some(hook) = self.hook.take() {
                let _ = UnhookWindowsHookEx(hook);
            }
            if let Some(window) = self.window.take() {
                let _ = DestroyWindow(window);
            }
        }
    }
}
fn run(
    label: &str,
    stop: &AtomicBool,
    state: impl Fn() -> (bool, bool),
    panic: impl Fn(),
) -> Result<(), String> {
    let mut desktop = nexus_desktop::ThreadDesktop::new();
    let mut resources = Resources::default();
    let label: Vec<u16> = label.encode_utf16().chain(Some(0)).collect();
    // SAFETY: module handle is borrowed for process lifetime; class callback remains available.
    let module = unsafe { GetModuleHandleW(None) }.map_err(|e| e.to_string())?;
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: module.into(),
        lpszClassName: w!("NexusActiveSession"),
        ..Default::default()
    };
    // SAFETY: class name and function pointer remain valid for process lifetime.
    let _ = unsafe { RegisterClassW(&class) };
    while !stop.load(Ordering::Relaxed) {
        let (active, stopping) = state();
        if stopping {
            break;
        }
        if PANIC.swap(false, Ordering::SeqCst) {
            panic();
        }
        if let Ok(input) = nexus_desktop::input_desktop_name() {
            if desktop.attached() != Some(input.as_str()) {
                // SetThreadDesktop rejects threads with windows/hooks. Tear both down first.
                resources = Resources::default();
                if let Err(e) = desktop.sync() {
                    tracing::debug!(error = %e, "waiting for session UI desktop");
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
            }
            if resources.hook.is_none() {
                // SAFETY: thread-owned hook and callback; messages are pumped below.
                resources.hook = Some(
                    unsafe {
                        SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), Some(module.into()), 0)
                    }
                    .map_err(|e| e.to_string())?,
                );
            }
            if active && input.eq_ignore_ascii_case("Default") {
                if resources.window.is_none() {
                    // SAFETY: valid registered class, module and terminated label; no remote
                    // text is rendered into this privileged window.
                    resources.window = Some(
                        unsafe {
                            CreateWindowExW(
                                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                                w!("NexusActiveSession"),
                                PCWSTR(label.as_ptr()),
                                WS_POPUP | WS_BORDER,
                                20,
                                20,
                                520,
                                40,
                                None,
                                None,
                                Some(module.into()),
                                None,
                            )
                        }
                        .map_err(|e| e.to_string())?,
                    );
                }
                if let Some(window) = resources.window {
                    // SAFETY: live owned window; show without taking focus from local/remote apps.
                    unsafe {
                        let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
                        let _ = SetWindowPos(
                            window,
                            Some(HWND_TOPMOST),
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                        );
                    }
                }
            } else if let Some(window) = resources.window.take() {
                // SAFETY: own live window, destroyed once.
                let _ = unsafe { DestroyWindow(window) };
            }
        } else {
            // Neither a badge nor a stale hook belongs on an inaccessible desktop.
            resources = Resources::default();
        }
        // SAFETY: initialized thread-local messages, callback and owned windows remain live.
        unsafe {
            let mut message = MSG::default();
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        std::thread::sleep(Duration::from_millis(8));
    }
    // Resources must be destroyed before ThreadDesktop restores the original desktop.
    drop(resources);
    Ok(())
}
unsafe extern "system" fn keyboard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: hook callback supplies a valid KBDLLHOOKSTRUCT for nonnegative codes.
        let event = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if event.vkCode == 0x1b
            && !event.flags.contains(LLKHF_INJECTED)
            && !event.flags.contains(LLKHF_UP)
        {
            // SAFETY: read-only queries of the physical modifier state.
            let chord = unsafe {
                GetAsyncKeyState(0x11) < 0
                    && GetAsyncKeyState(0x12) < 0
                    && GetAsyncKeyState(0x10) < 0
            };
            if chord {
                PANIC.store(true, Ordering::SeqCst);
                return LRESULT(1);
            }
        }
    }
    // SAFETY: original hook arguments; maintain the chain for all ordinary/injected input.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}
unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_LBUTTONUP || message == WM_CLOSE {
        PANIC.store(true, Ordering::SeqCst);
        return LRESULT(0);
    }
    if message == WM_PAINT {
        // SAFETY: live thread-owned window, paired BeginPaint/EndPaint and bounded text buffer.
        unsafe {
            use windows::Win32::Graphics::Gdi::*;
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(window, &mut paint);
            let mut rect = windows::Win32::Foundation::RECT::default();
            let _ = GetClientRect(window, &mut rect);
            let _ = FillRect(dc, &rect, HBRUSH(GetStockObject(BLACK_BRUSH).0));
            SetTextColor(dc, windows::Win32::Foundation::COLORREF(0x00ffffff));
            SetBkMode(dc, TRANSPARENT);
            let mut label = [0u16; 512];
            let length = GetWindowTextW(window, &mut label).max(0).cast_unsigned() as usize;
            DrawTextW(
                dc,
                &mut label[..length],
                &mut rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            let _ = EndPaint(window, &paint);
        }
        return LRESULT(0);
    }
    // SAFETY: delegates valid original message parameters to the OS default window procedure.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}
