//! Locking down the agent's state directory.
//!
//! `C:\ProgramData` is readable by every local user, and the agent's identity seed, trust store
//! and IPC token live under it. The service therefore resets the ACL on its own directory to
//! SYSTEM and Administrators only, with inheritance cut, every time it starts (idempotent).

use std::path::Path;
use std::process::Command;

use crate::ServiceError;

/// `icacls <dir> /inheritance:r /grant:r SYSTEM:(OI)(CI)F Administrators:(OI)(CI)F`.
pub fn restrict_to_system_and_admins(dir: &Path) -> Result<(), ServiceError> {
    let out = Command::new("icacls")
        .arg(dir)
        .args([
            "/inheritance:r",
            "/grant:r",
            "*S-1-5-18:(OI)(CI)F",     // SYSTEM
            "*S-1-5-32-544:(OI)(CI)F", // Administrators
        ])
        .output()
        .map_err(|e| ServiceError::Os {
            what: "icacls",
            detail: e.to_string(),
        })?;
    if out.status.success() {
        Ok(())
    } else {
        Err(ServiceError::Os {
            what: "icacls",
            detail: String::from_utf8_lossy(&out.stdout).trim().to_owned(),
        })
    }
}
