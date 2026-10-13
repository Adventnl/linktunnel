//! Replaces the state tree's DACL, including explicit inherited grants left by earlier installs.
//! Identity keys, trust and IPC tokens must be readable only by SYSTEM and Administrators.
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
    result
}
fn apply(path: &Path, descriptor: PSECURITY_DESCRIPTOR) -> Result<(), ServiceError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| os("inspect state path", e))?;
    if metadata.file_type().is_symlink() {
        return Err(os(
            "restrict state ACL",
            "state tree contains a reparse point",
        ));
    }
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
        return Err(os(
            "restrict state ACL",
            format!(
                "{}: Win32 error {}",
                path.display(),
                unsafe { GetLastError() }.0
            ),
        ));
    }
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
fn os(what: &'static str, detail: impl std::fmt::Display) -> ServiceError {
    ServiceError::Os {
        what,
        detail: detail.to_string(),
    }
}
