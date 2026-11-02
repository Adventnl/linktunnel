//! Named-pipe access for the desktop owner enrolled by the elevated installer.
use std::path::Path;

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::core::PCWSTR;

pub(crate) fn desktop_user(dir: &Path) -> std::io::Result<Option<String>> {
    let path = dir.join("desktop-user.sid");
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let sid = text.trim();
    if sid.len() > 184
        || !sid.starts_with("S-1-")
        || !sid[2..].bytes().all(|c| c.is_ascii_digit() || c == b'-')
    {
        return Err(std::io::Error::other("invalid enrolled desktop user SID"));
    }
    Ok(Some(sid.to_owned()))
}

pub(crate) fn create(
    name: &str,
    first: bool,
    user: Option<&str>,
) -> std::io::Result<NamedPipeServer> {
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true);
    let Some(user) = user else {
        return options.create(name);
    };
    // The enrolled user can use local controls, but cannot read the host's device keys.
    // No Everyone, Authenticated Users or network-client grant is made.
    let sddl: Vec<u16> = format!("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;{user})")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let length = u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>())
        .map_err(std::io::Error::other)?;
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: terminated SDDL, validated numeric SID, writable output pointer.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    }
    .map_err(std::io::Error::other)?;
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: length,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
    // SAFETY: the descriptor and attributes outlive CreateNamedPipe; Windows copies them.
    let result =
        unsafe { options.create_with_security_attributes_raw(name, (&raw mut attributes).cast()) };
    // SAFETY: the conversion allocated the descriptor with LocalAlloc; it is no longer used.
    let _ = unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
    result
}
