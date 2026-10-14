//! Captures Windows shortcuts that the window event queue never receives (Win, Alt+Tab,
//! Alt+Esc, Alt+F4, Ctrl+Esc). Ordinary typing and the viewer escape hotkeys use winit as usual.
use crate::session::Command;
use nexus_proto::wire::input::{InputEvent, KeyState, PAGE_KEYBOARD};
use std::collections::BTreeSet;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL};
use windows::Win32::UI::WindowsAndMessaging::*;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

struct State {
    window: isize,
    commands: UnboundedSender<Command>,
    held: BTreeSet<u16>,
}
static STATE: OnceLock<Mutex<Option<State>>> = OnceLock::new();

pub(super) struct KeyboardCapture {
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl KeyboardCapture {
    pub(super) fn start(
        window: &winit::window::Window,
        commands: UnboundedSender<Command>,
    ) -> Result<Self, String> {
        let handle = window.window_handle().map_err(|e| e.to_string())?;
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return Err("viewer does not have a Win32 window".into());
        };
        let state = STATE.get_or_init(|| Mutex::new(None));
        let mut state = state.lock().map_err(|e| e.to_string())?;
        if state.is_some() {
            return Err("keyboard capture already active".into());
        }
        *state = Some(State {
            window: handle.hwnd.get(),
            commands,
            held: BTreeSet::new(),
        });
        drop(state);
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker_stop = Arc::clone(&stop);
        let worker = match std::thread::Builder::new()
            .name("viewer-keyboard".into())
            .spawn(move || {
                // SAFETY: a process-lifetime callback, live module and thread-owned hook/message pump.
                let setup = unsafe {
                    GetModuleHandleW(None).and_then(|module| {
                        SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), Some(module.into()), 0)
                    })
                };
                let hook = match setup {
                    Ok(hook) => {
                        let _ = ready_tx.send(Ok(()));
                        hook
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                        return;
                    }
                };
                while !worker_stop.load(Ordering::Relaxed) {
                    // SAFETY: pumping only this worker thread's initialized message structures.
                    unsafe {
                        let mut message = MSG::default();
                        while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                            let _ = TranslateMessage(&message);
                            DispatchMessageW(&message);
                        }
                    }
                    std::thread::sleep(Duration::from_millis(4));
                }
                // SAFETY: installed on this worker and removed before the callback state is dropped.
                let _ = unsafe { UnhookWindowsHookEx(hook) };
            }) {
            Ok(worker) => worker,
            Err(e) => {
                if let Ok(mut state) = STATE.get_or_init(|| Mutex::new(None)).lock() {
                    *state = None;
                }
                return Err(e.to_string());
            }
        };
        let capture = Self {
            stop,
            worker: Some(worker),
        };
        match ready_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(())) => Ok(capture),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(e.to_string()),
        }
    }
}
impl Drop for KeyboardCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Some(state) = STATE.get()
            && let Ok(mut state) = state.lock()
            && let Some(state) = state.take()
        {
            let _ = state.commands.send(Command::Input(InputEvent::ReleaseAll));
        }
    }
}
unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: Windows supplies a valid KBDLLHOOKSTRUCT for nonnegative hook codes.
        let event = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if !event.flags.contains(LLKHF_INJECTED)
            && let Some(state) = STATE.get()
            && let Ok(mut state) = state.try_lock()
            && let Some(state) = state.as_mut()
        {
            // SAFETY: argument-less query; HWND is compared by value, never dereferenced.
            let focused = unsafe { GetForegroundWindow() } == HWND(state.window as *mut _);
            let alt = event.flags.contains(LLKHF_ALTDOWN);
            // SAFETY: query only; do not call SendInput from the hook callback.
            let ctrl = unsafe { GetAsyncKeyState(VK_CONTROL.0 as i32) } < 0;
            let usage = match event.vkCode {
                0x5b => Some(0xe3),
                0x5c => Some(0xe7),
                0x09 if alt || state.held.contains(&0x2b) => Some(0x2b),
                0x1b if alt || ctrl || state.held.contains(&0x29) => Some(0x29),
                0x73 if alt || state.held.contains(&0x3d) => Some(0x3d),
                _ => None,
            };
            // Preserve the local Ctrl+Alt+Shift+Esc panic chord for the controller.
            // Its Esc is handled by the UI/global shortcut, never captured here.
            // SAFETY: modifier state query without side effects.
            let shift = unsafe { GetAsyncKeyState(0x10) } < 0;
            if focused && !(event.vkCode == 0x1b && ctrl && alt && shift) {
                if let Some(usage) = usage {
                    let up = event.flags.contains(LLKHF_UP);
                    let key_state = if up {
                        state.held.remove(&usage);
                        KeyState::Up
                    } else if state.held.insert(usage) {
                        KeyState::Down
                    } else {
                        KeyState::Repeat
                    };
                    let _ = state.commands.send(Command::Input(InputEvent::Key {
                        page: PAGE_KEYBOARD,
                        usage,
                        state: key_state,
                        modifiers: Default::default(),
                    }));
                    return LRESULT(1);
                }
            } else if !focused && !state.held.is_empty() {
                state.held.clear();
                let _ = state.commands.send(Command::Input(InputEvent::ReleaseAll));
            }
        }
    }
    // SAFETY: forwards the original hook parameters; hhk is ignored for low-level hooks.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}
