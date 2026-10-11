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
            let tag = data_encoding::HEXLOWER
                .encode(&nexus_core_hash(runtime_dir.to_string_lossy().as_bytes())[..8]);
            IpcEndpoint::Pipe(format!(r"\\.\pipe\nexus-agent-{tag}"))
        };
        Self {
            endpoint,
            token_file,
        }
    }
}

#[cfg(not(unix))]
fn nexus_core_hash(data: &[u8]) -> [u8; 32] {
    // A tiny non-cryptographic fold is enough to derive a stable pipe name; uniqueness per user
    // directory is all that is needed, and the token (not the name) is what authenticates.
    let mut out = [0u8; 32];
    for (i, b) in data.iter().enumerate() {
        out[i % 32] = out[i % 32].wrapping_mul(31).wrapping_add(*b).rotate_left(1);
    }
    out
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
}
