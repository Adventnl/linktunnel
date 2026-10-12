//! Asking the person at the console for permission, from a session-0 or SYSTEM process.
//!
//! `WTSSendMessage` shows a message box on a chosen session's desktop and returns the answer or a
//! timeout, with no UI process of ours needed there. It cannot be shown on the secure desktop
//! (lock screen / UAC), so a request that arrives while the PC is locked is answered by the
//! timeout, which refuses: use a pre-authorised ("unattended") device for the lock screen.
//!
//! Verification status: type-checked for `x86_64-pc-windows-msvc`, not run (docs/STATUS.md).

#![allow(unsafe_code)]

use windows::Win32::System::RemoteDesktop::WTSSendMessageW;
use windows::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_DEFBUTTON2, MB_ICONQUESTION, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO,
    MESSAGEBOX_RESULT,
};
use windows::core::PCWSTR;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Shows a Yes/No box on `session`; `Some(true)` = Yes, `Some(false)` = No, `None` = no answer
/// within `timeout_secs` (or the box could not be shown). Blocks; call from a blocking thread.
#[must_use]
pub fn ask(session: u32, title: &str, message: &str, timeout_secs: u32) -> Option<bool> {
    let (t, m) = (wide(title), wide(message));
    let mut response = MESSAGEBOX_RESULT(0);
    // SAFETY: `t` and `m` are NUL-terminated and outlive the call; the byte lengths exclude the
    // terminator as the API expects; `response` is a valid out-pointer; `bwait` makes the call
    // synchronous so none of these locals are used after it returns.
    let shown = unsafe {
        WTSSendMessageW(
            None,
            session,
            PCWSTR(t.as_ptr()),
            u32::try_from((t.len() - 1) * 2).unwrap_or(0),
            PCWSTR(m.as_ptr()),
            u32::try_from((m.len() - 1) * 2).unwrap_or(0),
            MB_YESNO | MB_ICONQUESTION | MB_TOPMOST | MB_SETFOREGROUND | MB_DEFBUTTON2,
            timeout_secs,
            &mut response,
            true,
        )
    };
    match shown {
        Ok(()) if response == IDYES => Some(true),
        Ok(()) if response.0 == 7 => Some(false), // IDNO
        _ => None,
    }
}

/// The session this process runs in (where its own prompts should appear).
#[must_use]
pub fn own_session() -> Option<u32> {
    crate::launcher::active_console_session()
}
