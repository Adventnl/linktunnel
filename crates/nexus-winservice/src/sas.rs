//! A per-child, SYSTEM-only request event. The SCM service performs SendSAS while impersonating
//! its child's session token; a network-facing helper cannot directly simulate Ctrl+Alt+Del.
use crate::ServiceError;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, FreeLibrary, GetLastError, HANDLE, HLOCAL, LocalFree,
    WAIT_OBJECT_0,
};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    ImpersonateLoggedOnUser, PSECURITY_DESCRIPTOR, RevertToSelf, SECURITY_ATTRIBUTES,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::{PCWSTR, s, w};

fn os(what: &'static str, detail: impl std::fmt::Display) -> ServiceError {
    ServiceError::Os {
        what,
        detail: detail.to_string(),
    }
}

#[derive(Debug)]
pub(crate) struct Broker {
    event: HANDLE,
    pub(crate) name: String,
}
// SAFETY: event handles are process-wide kernel objects, waited on only by the service loop.
unsafe impl Send for Broker {}
impl Broker {
    pub(crate) fn new() -> Result<Self, ServiceError> {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|e| os("SAS event random name", e))?;
        let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let name = format!(r"Global\NexusSas-{suffix}");
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let attributes_size = u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>())
            .map_err(|e| os("SAS event attributes size", e))?;
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: static valid SDDL, valid output. Only LocalSystem can open this broker event.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                w!("D:P(A;;GA;;;SY)"),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .map_err(|e| os("SAS event ACL", e))?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: attributes_size,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        // SAFETY: attributes, descriptor and terminated name remain alive through CreateEventW.
        let created =
            unsafe { CreateEventW(Some(&attributes), false, false, PCWSTR(wide.as_ptr())) };
        // SAFETY: GetLastError must immediately follow creation to detect pre-existing objects.
        let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        // SAFETY: conversion allocated this descriptor with LocalAlloc.
        let _ = unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
        let event = created.map_err(|e| os("create SAS event", e))?;
        if existed {
            // SAFETY: event was opened above and is not retained.
            let _ = unsafe { CloseHandle(event) };
            return Err(os("create SAS event", "event name already exists"));
        }
        Ok(Self { event, name })
    }

    pub(crate) fn dispatch(&self, session_token: HANDLE) -> Result<(), ServiceError> {
        // SAFETY: live event handle; zero timeout consumes a pending auto-reset request.
        if unsafe { WaitForSingleObject(self.event, 0) } != WAIT_OBJECT_0 {
            return Ok(());
        }
        // SAFETY: DLL search is restricted to System32, preventing application-directory loading.
        let module = unsafe { LoadLibraryExW(w!("sas.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
            .map_err(|e| os("load System32 sas.dll", e))?;
        let result = (|| {
            // SAFETY: module remains loaded until after this call.
            let address = unsafe { GetProcAddress(module, s!("SendSAS")) }
                .ok_or_else(|| os("resolve SendSAS", "export missing"))?;
            // SAFETY: SendSAS has the documented VOID WINAPI(BOOL) ABI.
            let send: unsafe extern "system" fn(windows::core::BOOL) =
                unsafe { std::mem::transmute(address) };
            // SAFETY: the service owns this primary SYSTEM token, retargeted to the child session.
            unsafe { ImpersonateLoggedOnUser(session_token) }
                .map_err(|e| os("impersonate SAS target session", e))?;
            // SAFETY: this code runs in the SCM service, under the requested session's token.
            unsafe { send(false.into()) };
            // Reversion failure must stop this service thread rather than leave it impersonating.
            // SAFETY: balances the successful impersonation above.
            unsafe { RevertToSelf() }.map_err(|e| os("revert SAS impersonation", e))?;
            tracing::info!("secure attention sequence dispatched by service");
            Ok(())
        })();
        // SAFETY: no function pointer is used after this release.
        let _ = unsafe { FreeLibrary(module) };
        result
    }
}
impl Drop for Broker {
    fn drop(&mut self) {
        // SAFETY: this value owns the event handle exclusively.
        let _ = unsafe { CloseHandle(self.event) };
    }
}
