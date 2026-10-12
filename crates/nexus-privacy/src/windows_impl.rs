//! Isolated Win32 FFI. All HWND/hooks stay on one owned OS thread. Shields are excluded from
//! capture, injected remote events pass through, and physical Esc always releases privacy.
use crate::{PrivacyBackend, PrivacyError, PrivacyLease};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BLACK_BRUSH, BeginPaint, DT_CENTER, DT_SINGLELINE, DT_VCENTER, DrawTextW, EndPaint,
    GetStockObject, HBRUSH, PAINTSTRUCT, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;
static ACTIVE: AtomicBool = AtomicBool::new(false);
static BLOCK: AtomicBool = AtomicBool::new(false);
static ESCAPE: AtomicBool = AtomicBool::new(false);
pub(super) struct WindowsPrivacy;
struct Lease {
    tx: mpsc::Sender<bool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl PrivacyLease for Lease {
    fn renew(&self) -> Result<(), PrivacyError> {
        self.tx
            .send(true)
            .map_err(|_| PrivacyError::Failed("privacy ended locally".into()))
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let _ = self.tx.send(false);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
impl PrivacyBackend for WindowsPrivacy {
    fn enable(&self, block: bool) -> Result<Box<dyn PrivacyLease>, PrivacyError> {
        if ACTIVE.swap(true, Ordering::SeqCst) {
            return Err(PrivacyError::Busy);
        }
        ESCAPE.store(false, Ordering::SeqCst);
        BLOCK.store(block, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = match std::thread::Builder::new()
            .name("privacy-screen".into())
            .spawn(move || {
                let result = run(&rx, &ready_tx);
                if let Err(e) = result {
                    tracing::warn!(error=%e,"privacy ended");
                }
                ACTIVE.store(false, Ordering::SeqCst);
            }) {
            Ok(t) => t,
            Err(e) => {
                ACTIVE.store(false, Ordering::SeqCst);
                return Err(PrivacyError::Failed(e.to_string()));
            }
        };
        let lease = Lease {
            tx,
            thread: Some(thread),
        };
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Box::new(lease)),
            Ok(Err(e)) => Err(PrivacyError::Failed(e)),
            Err(e) => Err(PrivacyError::Failed(e.to_string())),
        }
    }
}
struct Resources {
    window: Option<HWND>,
    keyboard: Option<HHOOK>,
    mouse: Option<HHOOK>,
}
impl Drop for Resources {
    fn drop(&mut self) {
        // SAFETY: every handle was created on this same thread, is owned here and is freed once.
        unsafe {
            if let Some(h) = self.keyboard {
                let _ = UnhookWindowsHookEx(h);
            }
            if let Some(h) = self.mouse {
                let _ = UnhookWindowsHookEx(h);
            }
            if let Some(h) = self.window {
                let _ = DestroyWindow(h);
            }
        }
    }
}
fn run(
    rx: &mpsc::Receiver<bool>,
    ready: &mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let mut resources = Resources {
        window: None,
        keyboard: None,
        mouse: None,
    };
    let setup = (|| -> Result<(), String> {
        // Only the normal interactive desktop supports these shields. UAC/login cannot be hidden.
        let mut desktop = nexus_desktop::ThreadDesktop::new();
        desktop.sync().map_err(|e| e.to_string())?;
        if desktop.attached().is_some_and(|name| name != "Default") {
            return Err("privacy is unavailable on the secure desktop".into());
        }
        // SAFETY: all objects are local; static class strings and wndproc remain valid for process
        // lifetime, coordinates come from the OS, and a live window is owned by Resources.
        unsafe {
            let module = GetModuleHandleW(None).map_err(|e| e.to_string())?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: module.into(),
                lpszClassName: w!("NexusPrivacyShield"),
                hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
                ..Default::default()
            };
            let _ = RegisterClassW(&class);
            let window = CreateWindowExW(
                WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE
                    | WS_EX_TRANSPARENT
                    | WS_EX_LAYERED,
                w!("NexusPrivacyShield"),
                w!("Nexus Remote privacy — press Esc locally to restore"),
                WS_POPUP,
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
                None,
                None,
                Some(module.into()),
                None,
            )
            .map_err(|e| e.to_string())?;
            resources.window = Some(window);
            SetLayeredWindowAttributes(window, COLORREF(0), 255, LWA_ALPHA)
                .map_err(|e| e.to_string())?;
            SetWindowDisplayAffinity(window, WDA_EXCLUDEFROMCAPTURE).map_err(|e| e.to_string())?;
            resources.keyboard = Some(
                SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), Some(module.into()), 0)
                    .map_err(|e| e.to_string())?,
            );
            if BLOCK.load(Ordering::SeqCst) {
                resources.mouse = Some(
                    SetWindowsHookExW(WH_MOUSE_LL, Some(mouse), Some(module.into()), 0)
                        .map_err(|e| e.to_string())?,
                );
            }
            let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
        }
        Ok(())
    })();
    let _ = ready.send(setup.clone());
    setup?;
    let mut renewed = Instant::now();
    while !ESCAPE.load(Ordering::SeqCst) && renewed.elapsed() < Duration::from_secs(10) {
        if nexus_desktop::input_desktop_name().is_ok_and(|n| n != "Default") {
            break;
        }
        match rx.recv_timeout(Duration::from_millis(8)) {
            Ok(true) => renewed = Instant::now(),
            Ok(false) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        // SAFETY: pumping only this thread's initialized MSG structures; hooks must be pumped.
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    Ok(())
}
unsafe extern "system" fn keyboard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: Windows supplies a KBDLLHOOKSTRUCT valid for this callback when code >= 0.
        let event = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if !event.flags.contains(LLKHF_INJECTED) {
            if event.vkCode == 0x1b {
                ESCAPE.store(true, Ordering::SeqCst);
                return LRESULT(1);
            }
            if BLOCK.load(Ordering::SeqCst) {
                return LRESULT(1);
            }
        }
    }
    // SAFETY: forwards the original hook arguments; hhk is ignored for low-level hooks.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}
unsafe extern "system" fn mouse(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: Windows supplies MSLLHOOKSTRUCT for nonnegative hook codes.
        let event = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        if event.flags & LLMHF_INJECTED == 0 {
            return LRESULT(1);
        }
    }
    // SAFETY: forwards original hook args and maintains hook chain for injected events.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_PAINT {
        // SAFETY: HWND belongs to this message thread; initialized paint/rect structures are
        // written by Win32. Text is a mutable UTF-16 buffer valid until DrawTextW returns.
        unsafe {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut paint);
            let mut rect = windows::Win32::Foundation::RECT::default();
            let _ = GetClientRect(hwnd, &mut rect);
            SetTextColor(dc, COLORREF(0x00ffffff));
            SetBkMode(dc, TRANSPARENT);
            let label =
                nexus_core::i18n::tr(nexus_core::i18n::Locale::detect(), "privacy.local_restore");
            let mut label: Vec<u16> = label.encode_utf16().collect();
            DrawTextW(
                dc,
                &mut label,
                &mut rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            let _ = EndPaint(hwnd, &paint);
        }
        return LRESULT(0);
    }
    // SAFETY: forwards the original window message and its valid handle to the OS default handler.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}
