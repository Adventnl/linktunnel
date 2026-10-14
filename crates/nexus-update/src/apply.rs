//! Putting a verified file in place, and taking it back out.

use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::process::Command;

#[cfg(unix)]
use crate::manifest::UpdateError;

/// Replaces `target` (a program file) with `staged`, keeping the old one as `<target>.old`.
///
/// The swap is two renames in the same directory, so at no point is there a missing or half
/// written program; a crash between them leaves `<target>.old` to roll back from.
#[cfg(unix)]
pub fn replace_binary(staged: &Path, target: &Path) -> Result<PathBuf, UpdateError> {
    use std::os::unix::fs::PermissionsExt;
    let dir = target
        .parent()
        .ok_or_else(|| UpdateError::Io("the target has no directory".into()))?;
    let name = target
        .file_name()
        .ok_or_else(|| UpdateError::Io("the target has no file name".into()))?
        .to_string_lossy()
        .into_owned();
    // Same directory so the renames are atomic (never cross a filesystem boundary).
    let incoming = dir.join(format!("{name}.new"));
    std::fs::copy(staged, &incoming)?;
    std::fs::set_permissions(&incoming, std::fs::Permissions::from_mode(0o755))?;
    let old = dir.join(format!("{name}.old"));
    if target.exists() {
        std::fs::rename(target, &old)?;
    }
    if let Err(e) = std::fs::rename(&incoming, target) {
        // Put things back.
        let _ = std::fs::rename(&old, target);
        return Err(e.into());
    }
    Ok(old)
}

/// Undoes [`replace_binary`] using the `.old` copy.
#[cfg(unix)]
pub fn roll_back(target: &Path) -> Result<(), UpdateError> {
    let name = target
        .file_name()
        .ok_or_else(|| UpdateError::Io("the target has no file name".into()))?
        .to_string_lossy()
        .into_owned();
    let old = target.with_file_name(format!("{name}.old"));
    if !old.exists() {
        return Err(UpdateError::Io(
            "there is no previous version to go back to".into(),
        ));
    }
    let failed = target.with_file_name(format!("{name}.failed"));
    if target.exists() {
        std::fs::rename(target, &failed)?;
    }
    std::fs::rename(&old, target)?;
    let _ = std::fs::remove_file(failed);
    Ok(())
}

/// How to hand a staged installer to the operating system. Installers replace running programs
/// themselves (stopping the service first), which a process cannot do to its own executable on
/// Windows.
#[must_use]
pub fn installer_command(os: &str, kind: &str, staged: &Path) -> Option<Command> {
    let p = staged.to_string_lossy().into_owned();
    let mut cmd = match (os, kind) {
        ("windows", "msi") => {
            let mut c = Command::new("msiexec");
            c.args(["/i", &p, "/qn", "/norestart"]);
            c
        }
        ("macos", "pkg") => {
            let mut c = Command::new("installer");
            c.args(["-pkg", &p, "-target", "/"]);
            c
        }
        ("linux", "deb") => {
            let mut c = Command::new("dpkg");
            c.args(["-i", &p]);
            c
        }
        ("linux", "rpm") => {
            let mut c = Command::new("rpm");
            c.args(["-U", &p]);
            c
        }
        _ => return None,
    };
    cmd.stdin(std::process::Stdio::null());
    Some(cmd)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn replacing_keeps_the_old_one_and_rolling_back_restores_it() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nexus-agent");
        std::fs::write(&target, b"v1").unwrap();
        let staged = dir.path().join("download.bin");
        std::fs::write(&staged, b"v2").unwrap();

        let old = replace_binary(&staged, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"v2");
        assert_eq!(std::fs::read(&old).unwrap(), b"v1");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o111,
            0o111
        );

        roll_back(&target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"v1");
        assert!(roll_back(&target).is_err(), "nothing left to roll back to");
    }

    #[test]
    fn installing_over_nothing_works_and_a_missing_directory_fails_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("s");
        std::fs::write(&staged, b"x").unwrap();
        let fresh = dir.path().join("fresh");
        replace_binary(&staged, &fresh).unwrap();
        assert_eq!(std::fs::read(&fresh).unwrap(), b"x");
        assert!(replace_binary(&staged, &dir.path().join("no/such/dir/prog")).is_err());
    }

    #[test]
    fn installer_commands_exist_for_the_known_kinds_only() {
        let p = Path::new("/tmp/x");
        for (os, kind, prog) in [
            ("windows", "msi", "msiexec"),
            ("macos", "pkg", "installer"),
            ("linux", "deb", "dpkg"),
            ("linux", "rpm", "rpm"),
        ] {
            let c = installer_command(os, kind, p).unwrap();
            assert_eq!(c.get_program(), prog);
        }
        assert!(installer_command("linux", "exe", p).is_none());
        assert!(installer_command("windows", "deb", p).is_none());
    }
}
