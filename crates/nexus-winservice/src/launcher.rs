//! Starting the agent as SYSTEM inside the active console session.
//!
//! The recipe is the standard one for a service that must put UI-capable processes on a user's
//! desktop: duplicate the service's own SYSTEM token, retarget the copy at the console session
//! (`TokenSessionId`, which needs `SeTcbPrivilege`, held by LocalSystem services), build the
//! environment block for it, and `CreateProcessAsUser` on `winsta0\default`.
//!
//! Verification status: type-checked for `x86_64-pc-windows-msvc`, not run (docs/STATUS.md).

#![allow(unsafe_code)]

use std::path::Path;
use std::time::Instant;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Security::{
    DuplicateTokenEx, SecurityImpersonation, SetTokenInformation, TOKEN_ALL_ACCESS, TokenPrimary,
    TokenSessionId,
};
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, GetCurrentProcess,
    GetExitCodeProcess, OpenProcessToken, PROCESS_INFORMATION, STARTUPINFOW, TerminateProcess,
    WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

use crate::ServiceError;
use crate::cmdline::command_line;

fn os(what: &'static str, e: impl std::fmt::Display) -> ServiceError {
    ServiceError::Os {
        what,
        detail: e.to_string(),
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The session whose desktop is on the physical console (or the one an RDP user took over the
/// console with), or `None` while Windows is switching users.
#[must_use]
pub fn active_console_session() -> Option<u32> {
    // SAFETY: argument-less query.
    let id = unsafe { WTSGetActiveConsoleSessionId() };
    (id != 0xFFFF_FFFF).then_some(id)
}

/// A running child agent.
#[derive(Debug)]
pub struct Child {
    process: HANDLE,
    /// The session it was started in.
    pub session: u32,
    started: Instant,
}

// SAFETY: a process handle is a process-wide kernel object reference; any thread may wait on it.
unsafe impl Send for Child {}

impl Child {
    /// True once the process has ended.
    #[must_use]
    pub fn has_exited(&self) -> bool {
        // SAFETY: `process` is a live handle owned by this value.
        unsafe { WaitForSingleObject(self.process, 0) == WAIT_OBJECT_0 }
    }

    /// How long it has been running.
    #[must_use]
    pub fn ran_for(&self) -> std::time::Duration {
        self.started.elapsed()
    }

    /// The exit code, if it has ended.
    #[must_use]
    pub fn exit_code(&self) -> Option<u32> {
        let mut code = 0u32;
        // SAFETY: valid handle and out-pointer.
        unsafe { GetExitCodeProcess(self.process, &mut code) }.ok()?;
        (code != 259).then_some(code) // 259 = STILL_ACTIVE
    }

    /// Terminates the process. The agent holds nothing that needs a graceful shutdown beyond
    /// what its next start recovers from (its state files are written atomically).
    pub fn kill(&self) {
        // SAFETY: valid handle; failure (already exited) is fine.
        let _ = unsafe { TerminateProcess(self.process, 1) };
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by this value and closed exactly once.
        let _ = unsafe { CloseHandle(self.process) };
    }
}

/// Closes a handle when dropped.
struct Owned(HANDLE);
impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Starts `exe args…` as SYSTEM in `session`.
pub fn launch(exe: &Path, args: &[String], session: u32) -> Result<Child, ServiceError> {
    // SAFETY: every pointer passed below refers to a live local; every handle opened here is
    // either returned inside `Child` or closed by `Owned`.
    unsafe {
        let mut own = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &mut own)
            .map_err(|e| os("OpenProcessToken", e))?;
        let own = Owned(own);

        let mut primary = HANDLE::default();
        DuplicateTokenEx(
            own.0,
            TOKEN_ALL_ACCESS,
            None,
            SecurityImpersonation,
            TokenPrimary,
            &mut primary,
        )
        .map_err(|e| os("DuplicateTokenEx", e))?;
        let primary = Owned(primary);

        SetTokenInformation(
            primary.0,
            TokenSessionId,
            std::ptr::from_ref(&session).cast(),
            u32::try_from(std::mem::size_of::<u32>()).unwrap_or(4),
        )
        .map_err(|e| os("SetTokenInformation(TokenSessionId)", e))?;

        let mut env: *mut core::ffi::c_void = std::ptr::null_mut();
        CreateEnvironmentBlock(&mut env, Some(primary.0), false)
            .map_err(|e| os("CreateEnvironmentBlock", e))?;

        let mut desktop = wide(r"winsta0\default");
        let startup = STARTUPINFOW {
            cb: u32::try_from(std::mem::size_of::<STARTUPINFOW>()).unwrap_or(0),
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            ..Default::default()
        };
        let exe_str = exe.display().to_string();
        let mut cmd = wide(&command_line(&exe_str, args));
        let exe_w = wide(&exe_str);
        let mut info = PROCESS_INFORMATION::default();
        let created = CreateProcessAsUserW(
            Some(primary.0),
            PCWSTR(exe_w.as_ptr()),
            Some(PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
            Some(env.cast_const()),
            PCWSTR::null(),
            &startup,
            &mut info,
        );
        let _ = DestroyEnvironmentBlock(env);
        created.map_err(|e| os("CreateProcessAsUser", e))?;
        // The primary thread handle is not needed.
        let _ = CloseHandle(info.hThread);
        Ok(Child {
            process: info.hProcess,
            session,
            started: Instant::now(),
        })
    }
}
