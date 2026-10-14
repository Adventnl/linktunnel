use std::path::{Path, PathBuf};

/// Where an agent listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcEndpoint {
    /// Unix domain socket path.
    Unix(PathBuf),
    /// Windows named pipe (`\\.\pipe\…`).
    Pipe(String),
}

/// Locations of the agent's IPC endpoint and its token file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpcPaths {
    /// The socket / pipe.
    pub endpoint: IpcEndpoint,
    /// File holding the authentication token.
    pub token_file: PathBuf,
}

impl IpcPaths {
    /// Layout inside the user's private runtime directory.
    #[must_use]
    pub fn in_dir(runtime_dir: &Path) -> Self {
        let token_file = runtime_dir.join("agent.token");
        #[cfg(unix)]
        let endpoint = IpcEndpoint::Unix(runtime_dir.join("agent.sock"));
        #[cfg(not(unix))]
        let endpoint = {
            // One pipe per runtime directory (i.e. per user), without exposing the user name.
            let hash = blake3::hash(runtime_dir.to_string_lossy().as_bytes());
            let tag = data_encoding::HEXLOWER.encode(&hash.as_bytes()[..16]);
            IpcEndpoint::Pipe(format!(r"\\.\pipe\nexus-agent-{tag}"))
        };
        Self {
            endpoint,
            token_file,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_live_in_the_runtime_dir() {
        let p = IpcPaths::in_dir(Path::new("/tmp/x/run"));
        assert_eq!(p.token_file, Path::new("/tmp/x/run/agent.token"));
        #[cfg(unix)]
        assert_eq!(
            p.endpoint,
            IpcEndpoint::Unix(PathBuf::from("/tmp/x/run/agent.sock"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn long_windows_paths_with_different_suffixes_have_distinct_pipes() {
        let base = r"C:\Users\Adventnl\AppData\Local\Temp";
        let a = IpcPaths::in_dir(&Path::new(base).join(".tmpAlpha").join("run"));
        let b = IpcPaths::in_dir(&Path::new(base).join(".tmpBravo").join("run"));
        assert_ne!(a.endpoint, b.endpoint);
        assert_eq!(
            a.endpoint,
            IpcPaths::in_dir(&Path::new(base).join(".tmpAlpha").join("run")).endpoint
        );
    }
}
