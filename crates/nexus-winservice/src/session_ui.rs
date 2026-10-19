//! Physical panic shortcut, independent of the Tauri dashboard.
//! The hook follows desktops on its own thread without creating an overlay window.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::*;
static PANIC: AtomicBool = AtomicBool::new(false);

/// Owns the target's physical panic thread and stops it before the agent is dropped.
pub struct SessionUi {
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl SessionUi {
    /// `stopping` returns whether the agent is shutting down. The physical hotkey calls
    /// `panic` to disconnect sessions and disable future access, including on Winlogon.
    /// Session visibility and ordinary disconnect controls remain in the dashboard.
    pub fn start(
        stopping: impl Fn() -> bool + Send + 'static,
        panic: impl Fn() + Send + 'static,
    ) -> Result<Self, std::io::Error> {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = std::thread::Builder::new()
            .name("session-panic".into())
            .spawn(move || {
                if let Err(e) = run(&worker_stop, stopping, panic) {
                    tracing::warn!(error = %e, "Windows physical panic shortcut unavailable");
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
    hook: Option<HHOOK>,
}
impl Drop for Resources {
    fn drop(&mut self) {
        // SAFETY: owned handles created on this UI thread, removed before changing desktops.
        unsafe {
            if let Some(hook) = self.hook.take() {
                let _ = UnhookWindowsHookEx(hook);
            }
        }
    }
}
fn run(stop: &AtomicBool, stopping: impl Fn() -> bool, panic: impl Fn()) -> Result<(), String> {
    let mut desktop = nexus_desktop::ThreadDesktop::new();
    let mut resources = Resources::default();
    // SAFETY: module handle is borrowed for process lifetime.
    let module = unsafe { GetModuleHandleW(None) }.map_err(|e| e.to_string())?;
    while !stop.load(Ordering::Relaxed) {
        if stopping() {
            break;
        }
        if PANIC.swap(false, Ordering::SeqCst) {
            panic();
        }
        if let Ok(input) = nexus_desktop::input_desktop_name() {
            if desktop.attached() != Some(input.as_str()) {
                // SetThreadDesktop rejects threads with hooks. Remove the old hook first.
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
        } else {
            // A stale hook does not belong on an inaccessible desktop.
            resources = Resources::default();
        }
        // SAFETY: initialized thread-local messages and hook callback remain live.
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
