//! Replaces the state tree's DACL, including explicit inherited grants left by earlier installs.
//! Device keys stay admin-only; an explicitly enrolled desktop owner may read runtime IPC.
#![allow(unsafe_code)]
use crate::ServiceError;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows::Win32::Foundation::{GetLastError, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    SetFileSecurityW,
};
use windows::core::{PCWSTR, w};

/// Applies an exact protected SYSTEM + Administrators DACL to every state file/directory.
/// Reparse points are refused rather than followed into a location outside the state tree.
pub fn restrict_to_system_and_admins(dir: &Path) -> Result<(), ServiceError> {
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: constant valid SDDL; the descriptor is freed after all filesystem calls.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            w!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    }
    .map_err(|e| os("state ACL descriptor", e))?;
    let result = apply(dir, descriptor);
    // SAFETY: conversion allocated this descriptor with LocalAlloc, now no longer borrowed.
    let _ = unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
    result?;
    grant_desktop_runtime(dir)
}

fn grant_desktop_runtime(dir: &Path) -> Result<(), ServiceError> {
    let runtime = dir.join("run");
    let sid = match std::fs::read_to_string(runtime.join("desktop-user.sid")) {
        Ok(sid) => sid,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(os("read desktop owner", e)),
    };
    let sid = sid.trim();
    if sid.len() > 184
        || !sid.starts_with("S-1-")
        || !sid[2..].bytes().all(|c| c.is_ascii_digit() || c == b'-')
    {
        return Err(os("desktop owner", "invalid SID"));
    }
    // Only this directory gets traversal. No owner ACE is inherited by config/data/keys.
    for (path, grants, recursive) in [
        (dir, format!("(A;;GRGX;;;{sid})"), false),
        (runtime.as_path(), format!("(A;OICI;GRGX;;;{sid})"), true),
    ] {
        let sddl: Vec<u16> = format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA){grants}")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: numeric SID, terminated SDDL and valid output pointer.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .map_err(|e| os("desktop runtime ACL", e))?;
        let result = if recursive {
            apply(path, descriptor)
        } else {
            set_acl(path, descriptor)
        };
        // SAFETY: conversion owns this allocation, which is no longer borrowed.
        let _ = unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
        result?;
    }
    Ok(())
}
fn apply(path: &Path, descriptor: PSECURITY_DESCRIPTOR) -> Result<(), ServiceError> {
    use std::os::windows::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(path).map_err(|e| os("inspect state path", e))?;
    if metadata.file_attributes() & 0x400 != 0 {
        // FILE_ATTRIBUTE_REPARSE_POINT, including junctions.
        return Err(os(
            "restrict state ACL",
            "state tree contains a reparse point",
        ));
    }
    set_acl(path, descriptor)?;
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path).map_err(|e| os("enumerate state files", e))? {
            apply(
                &entry.map_err(|e| os("inspect state entry", e))?.path(),
                descriptor,
            )?;
        }
    }
    Ok(())
}
fn set_acl(path: &Path, descriptor: PSECURITY_DESCRIPTOR) -> Result<(), ServiceError> {
    let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: path is terminated and descriptor remains valid throughout this traversal.
    if !unsafe {
        SetFileSecurityW(
            PCWSTR(name.as_ptr()),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        )
    }
    .as_bool()
    {
        // SAFETY: capture the error immediately after the failing call.
        let error = unsafe { GetLastError() }.0;
        return Err(os(
            "restrict state ACL",
            format!("{}: Win32 error {error}", path.display()),
        ));
    }
    Ok(())
}
fn os(what: &'static str, detail: impl std::fmt::Display) -> ServiceError {
    ServiceError::Os {
        what,
        detail: detail.to_string(),
    }
}
