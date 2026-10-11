//! Per-OS locations for configuration, state, logs and runtime files.
//!
//! Two scopes exist because a Nexus agent runs in two very different places:
//!
//! * [`Scope::User`]: the desktop app / CLI of an interactive user.
//! * [`Scope::System`]: the boot-time service (`SYSTEM` on Windows, `root` LaunchDaemon on
//!   macOS, a dedicated systemd user on Linux).
//!
//! Setting `NEXUS_HOME=/some/dir` relocates everything under one root (used by tests,
//! containers and portable installs).

use std::path::{Path, PathBuf};

use directories::ProjectDirs;

/// Which identity a process runs as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// An interactive user session.
    User,
    /// The machine-wide service account.
    System,
}

/// Resolved directory layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Hand-edited configuration (`config.toml`).
    pub config_dir: PathBuf,
    /// Durable state: identity fallback file, pairing database, audit log.
    pub data_dir: PathBuf,
    /// Log files.
    pub log_dir: PathBuf,
    /// Sockets / pid files / other runtime-only files.
    pub runtime_dir: PathBuf,
}

/// Errors resolving the directory layout.
#[derive(Debug, thiserror::Error)]
pub enum PathsError {
    /// The OS did not report a home directory (very unusual: no `$HOME`, no passwd entry).
    #[error("could not determine a home directory for the current user")]
    NoHome,
}

/// Environment variable that relocates all directories under a single root.
pub const HOME_ENV: &str = "NEXUS_HOME";

impl Paths {
    /// Resolves the layout for `scope`, honouring [`HOME_ENV`].
    pub fn resolve(scope: Scope) -> Result<Self, PathsError> {
        if let Some(root) = std::env::var_os(HOME_ENV).filter(|v| !v.is_empty()) {
            return Ok(Self::under(Path::new(&root)));
        }
        match scope {
            Scope::User => Self::user_default(),
            Scope::System => Ok(Self::system_default()),
        }
    }

    /// Layout rooted at `root`: `config/`, `data/`, `logs/`, `run/`.
    #[must_use]
    pub fn under(root: &Path) -> Self {
        Self {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            log_dir: root.join("logs"),
            runtime_dir: root.join("run"),
        }
    }

    fn user_default() -> Result<Self, PathsError> {
        let dirs = ProjectDirs::from("app", "nexus-remote", "nexus").ok_or(PathsError::NoHome)?;
        let state = dirs
            .state_dir()
            .map_or_else(|| dirs.data_local_dir().join("state"), Path::to_path_buf);
        Ok(Self {
            config_dir: dirs.config_dir().to_path_buf(),
            data_dir: dirs.data_dir().to_path_buf(),
            log_dir: state.join("logs"),
            runtime_dir: dirs
                .runtime_dir()
                .map_or_else(|| dirs.cache_dir().join("run"), Path::to_path_buf),
        })
    }

    #[cfg(target_os = "windows")]
    fn system_default() -> Self {
        let root = std::env::var_os("ProgramData")
            .map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from)
            .join("Nexus");
        Self::under(&root)
    }

    #[cfg(target_os = "macos")]
    fn system_default() -> Self {
        Self {
            config_dir: PathBuf::from("/Library/Application Support/Nexus/config"),
            data_dir: PathBuf::from("/Library/Application Support/Nexus/data"),
            log_dir: PathBuf::from("/Library/Logs/Nexus"),
            runtime_dir: PathBuf::from("/var/run/nexus"),
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    fn system_default() -> Self {
        Self {
            config_dir: PathBuf::from("/etc/nexus"),
            data_dir: PathBuf::from("/var/lib/nexus"),
            log_dir: PathBuf::from("/var/log/nexus"),
            runtime_dir: PathBuf::from("/run/nexus"),
        }
    }

    /// Default config file location.
    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    /// Creates every directory in the layout with owner-only permissions on Unix.
    pub fn ensure(&self) -> std::io::Result<()> {
        for dir in [
            &self.config_dir,
            &self.data_dir,
            &self.log_dir,
            &self.runtime_dir,
        ] {
            create_private_dir(dir)?;
        }
        Ok(())
    }
}

/// `mkdir -p` that makes any directory it *creates* private to the owner (0700) on Unix.
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_builds_expected_layout() {
        let p = Paths::under(Path::new("/x"));
        assert_eq!(p.config_dir, Path::new("/x/config"));
        assert_eq!(p.data_dir, Path::new("/x/data"));
        assert_eq!(p.log_dir, Path::new("/x/logs"));
        assert_eq!(p.runtime_dir, Path::new("/x/run"));
        assert_eq!(p.config_file(), Path::new("/x/config/config.toml"));
    }

    #[test]
    fn system_and_user_scopes_differ() {
        let user = Paths::user_default().unwrap();
        let system = Paths::system_default();
        assert_ne!(user.data_dir, system.data_dir);
    }

    #[test]
    fn ensure_creates_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Paths::under(tmp.path());
        p.ensure().unwrap();
        assert!(p.config_dir.is_dir() && p.data_dir.is_dir());
        assert!(p.log_dir.is_dir() && p.runtime_dir.is_dir());
        // Idempotent.
        p.ensure().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn ensure_makes_private_dirs() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let p = Paths::under(&tmp.path().join("fresh"));
        p.ensure().unwrap();
        let mode = std::fs::metadata(&p.data_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "data dir must be owner-only");
    }
}
