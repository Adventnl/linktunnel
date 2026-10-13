//! Starting the agent as SYSTEM inside the active console session.
//!
//! The recipe is the standard one for a service that must put UI-capable processes on a user's
//! desktop: duplicate the service's own SYSTEM token, retarget the copy at the console session
//! (`TokenSessionId`, which needs `SeTcbPrivilege`, held by LocalSystem services), build the
//! environment block for it, and `CreateProcessAsUser` on `winsta0\default`.
//!
//! Verification status: source-only Windows revision; no builds or tests (docs/STATUS.md).

#![allow(unsafe_code)]

use std::path::Path;
use std::time::Instant;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_NOT_ALL_ASSIGNED, GetLastError, HANDLE, LUID, WAIT_OBJECT_0,
};
use windows::Win32::Security::{
    AdjustTokenPrivileges, DuplicateTokenEx, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
    SE_PRIVILEGE_ENABLED, SecurityImpersonation, SetTokenInformation, TOKEN_ALL_ACCESS,
    TOKEN_PRIVILEGES, TokenPrimary, TokenSessionId,
};
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows::Win32::System::RemoteDesktop::{
    WTS_SESSION_INFOW, WTSActive, WTSEnumerateSessionsW, WTSFreeMemory,
    WTSGetActiveConsoleSessionId,
};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW,
    GetCurrentProcess, GetExitCodeProcess, OpenProcessToken, PROCESS_INFORMATION, ResumeThread,
    STARTUPINFOW, TerminateProcess, WaitForSingleObject,
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
    token: Owned,
    sas: crate::sas::Broker,
    _job: Owned,
}

// SAFETY: a process handle is a process-wide kernel object reference; any thread may wait on it.
unsafe impl Send for Child {}

impl Child {
    /// Dispatches a helper's secure-attention request in this child's Windows session.
    pub fn dispatch_secure_attention(&self) -> Result<(), ServiceError> {
        self.sas.dispatch(self.token.0)
    }

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
        // The SCM process can exit unexpectedly as well as on an explicit stop. Do not orphan
        // the privileged session agent when its supervisor disappears.
        if !self.has_exited() {
            self.kill();
            // SAFETY: live process handle; bounded wait for termination and OS resource cleanup.
            let _ = unsafe { WaitForSingleObject(self.process, 5000) };
        }
        // SAFETY: the handle is owned by this value and closed exactly once.
        let _ = unsafe { CloseHandle(self.process) };
    }
}

/// Closes a handle when dropped.
#[derive(Debug)]
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
        for privilege in [
            "SeTcbPrivilege",
            "SeAssignPrimaryTokenPrivilege",
            "SeIncreaseQuotaPrivilege",
            "SeImpersonatePrivilege",
        ] {
            enable_privilege(own.0, privilege)?;
        }

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

        let sas = crate::sas::Broker::new()?;
        let job =
            Owned(CreateJobObjectW(None, PCWSTR::null()).map_err(|e| os("CreateJobObject", e))?);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&limits).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
        .map_err(|e| os("SetInformationJobObject(kill on supervisor exit)", e))?;
        let mut env: *mut core::ffi::c_void = std::ptr::null_mut();
        CreateEnvironmentBlock(&mut env, Some(primary.0), false)
            .map_err(|e| os("CreateEnvironmentBlock", e))?;

        let mut child_args = args.to_vec();
        child_args.extend(["--windows-sas-event".into(), sas.name.clone()]);
        let mut desktop = wide(r"winsta0\default");
        let startup = STARTUPINFOW {
            cb: u32::try_from(std::mem::size_of::<STARTUPINFOW>()).unwrap_or(0),
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            ..Default::default()
        };
        let exe_str = exe.display().to_string();
        let mut cmd = wide(&command_line(&exe_str, &child_args));
        let exe_w = wide(&exe_str);
        let mut info = PROCESS_INFORMATION::default();
        let created = CreateProcessAsUserW(
            Some(primary.0),
            PCWSTR(exe_w.as_ptr()),
            Some(PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW | CREATE_SUSPENDED,
            Some(env.cast_const()),
            PCWSTR::null(),
            &startup,
            &mut info,
        );
        let _ = DestroyEnvironmentBlock(env);
        created.map_err(|e| os("CreateProcessAsUser", e))?;
        // Put the child in a kill-on-close job before running any network/UI code. Even a hard
        // crash/taskkill of the service then tears down the child and its descendants.
        if let Err(e) = AssignProcessToJobObject(job.0, info.hProcess) {
            let _ = TerminateProcess(info.hProcess, 1);
            let _ = CloseHandle(info.hThread);
            let _ = CloseHandle(info.hProcess);
            return Err(os("AssignProcessToJobObject", e));
        }
        if ResumeThread(info.hThread) == u32::MAX {
            let error = GetLastError();
            let _ = TerminateProcess(info.hProcess, 1);
            let _ = CloseHandle(info.hThread);
            let _ = CloseHandle(info.hProcess);
            return Err(os("ResumeThread", format!("Win32 error {}", error.0)));
        }
        let _ = CloseHandle(info.hThread);
        Ok(Child {
            process: info.hProcess,
            session,
            started: Instant::now(),
            token: primary,
            sas,
            _job: job,
        })
    }
}

/// Prefer the physical console when active; follow one active RDP session when the console is
/// disconnected. This controls an existing licensed session and never creates Windows sessions.
pub fn interactive_session() -> Option<u32> {
    let console = active_console_session();
    let mut sessions: *mut WTS_SESSION_INFOW = std::ptr::null_mut();
    let mut count = 0;
    // SAFETY: WTS allocates the output array, released below with WTSFreeMemory.
    if unsafe { WTSEnumerateSessionsW(None, 0, 1, &mut sessions, &mut count) }.is_err() {
        return console;
    }
    let result = if sessions.is_null() || count == 0 {
        console
    } else {
        // SAFETY: successful enumeration returned count initialized entries.
        let entries = unsafe { std::slice::from_raw_parts(sessions, count as usize) };
        let mut active: Vec<u32> = entries
            .iter()
            .filter(|entry| entry.SessionId != 0 && entry.State == WTSActive)
            .map(|entry| entry.SessionId)
            .collect();
        active.sort_unstable();
        if console.is_some_and(|id| active.contains(&id)) {
            console
        } else {
            active.first().copied().or(console)
        }
    };
    // SAFETY: the WTS allocation is no longer borrowed.
    unsafe { WTSFreeMemory(sessions.cast()) };
    result
}

fn enable_privilege(token: HANDLE, name: &str) -> Result<(), ServiceError> {
    let name = wide(name);
    let mut luid = LUID::default();
    // SAFETY: terminated name, valid output and live service token.
    unsafe { LookupPrivilegeValueW(PCWSTR::null(), PCWSTR(name.as_ptr()), &mut luid) }
        .map_err(|e| os("look up service privilege", e))?;
    let privileges = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    // SAFETY: initialized single-entry privilege buffer and live token.
    unsafe { AdjustTokenPrivileges(token, false, Some(&privileges), 0, None, None) }
        .map_err(|e| os("enable service privilege", e))?;
    // SAFETY: AdjustTokenPrivileges can succeed while setting ERROR_NOT_ALL_ASSIGNED.
    if unsafe { GetLastError() } == ERROR_NOT_ALL_ASSIGNED {
        return Err(os(
            "enable service privilege",
            "required privilege is absent from the token",
        ));
    }
    Ok(())
}
