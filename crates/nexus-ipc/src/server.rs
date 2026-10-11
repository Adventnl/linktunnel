use std::time::Duration;

use nexus_proto::v1::{IpcFrame, IpcWelcome, ipc_frame::Body};
use subtle::ConstantTimeEq;
use tokio::time::timeout;
use zeroize::Zeroizing;

use crate::IpcError;
use crate::conn::{BoxedStream, IpcConnection};
use crate::paths::{IpcEndpoint, IpcPaths};

/// How long a client gets to present its token.
const AUTH_TIMEOUT: Duration = Duration::from_secs(5);

enum Listener {
    #[cfg(unix)]
    Unix(tokio::net::UnixListener),
    #[cfg(windows)]
    Pipe {
        name: String,
        next: tokio::net::windows::named_pipe::NamedPipeServer,
    },
}

/// The agent's IPC listener.
pub struct IpcServer {
    listener: Listener,
    token: Zeroizing<String>,
    paths: IpcPaths,
}

impl std::fmt::Debug for IpcServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IpcServer")
            .field("endpoint", &self.paths.endpoint)
            .finish_non_exhaustive()
    }
}

/// A connection that has been accepted but not yet authenticated.
pub struct PendingConnection {
    stream: BoxedStream,
    token: Zeroizing<String>,
}

fn new_token() -> Result<Zeroizing<String>, IpcError> {
    let mut raw = Zeroizing::new([0u8; 32]);
    getrandom::fill(&mut raw[..])
        .map_err(|e| IpcError::Io(std::io::Error::other(e.to_string())))?;
    Ok(Zeroizing::new(data_encoding::HEXLOWER.encode(&raw[..])))
}

fn write_private(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    f.write_all(contents)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)
}

impl IpcServer {
    /// Creates the endpoint and the token file. Fails with [`IpcError::AlreadyRunning`] if another
    /// agent is live on the same endpoint (a stale socket left by a crash is removed).
    pub async fn bind(paths: IpcPaths) -> Result<Self, IpcError> {
        if let Some(dir) = paths.token_file.parent() {
            create_private_dir(dir)?;
        }
        let token = new_token()?;

        #[cfg(unix)]
        let listener = {
            let IpcEndpoint::Unix(path) = &paths.endpoint else {
                return Err(IpcError::Protocol(
                    "unix endpoint required on this platform",
                ));
            };
            if path.as_os_str().len() >= 100 {
                return Err(IpcError::Io(std::io::Error::other(format!(
                    "socket path is too long for the OS ({} bytes): {}",
                    path.as_os_str().len(),
                    path.display()
                ))));
            }
            if path.exists() {
                match tokio::net::UnixStream::connect(path).await {
                    Ok(_) => return Err(IpcError::AlreadyRunning(path.display().to_string())),
                    Err(_) => std::fs::remove_file(path)?, // stale socket from a crashed agent
                }
            }
            let listener = tokio::net::UnixListener::bind(path)?;
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            }
            Listener::Unix(listener)
        };
        #[cfg(windows)]
        let listener = {
            use tokio::net::windows::named_pipe::ServerOptions;
            let IpcEndpoint::Pipe(name) = &paths.endpoint else {
                return Err(IpcError::Protocol(
                    "pipe endpoint required on this platform",
                ));
            };
            let first = ServerOptions::new()
                .first_pipe_instance(true)
                .reject_remote_clients(true)
                .create(name)
                .map_err(|_| IpcError::AlreadyRunning(name.clone()))?;
            Listener::Pipe {
                name: name.clone(),
                next: first,
            }
        };

        write_private(&paths.token_file, token.as_bytes())?;
        Ok(Self {
            listener,
            token,
            paths,
        })
    }

    /// The endpoint this server listens on.
    #[must_use]
    pub fn paths(&self) -> &IpcPaths {
        &self.paths
    }

    /// Waits for the next connection (not yet authenticated).
    pub async fn accept(&mut self) -> Result<PendingConnection, IpcError> {
        let token = self.token.clone();
        match &mut self.listener {
            #[cfg(unix)]
            Listener::Unix(listener) => loop {
                let (stream, _) = listener.accept().await?;
                // Only the owner of the agent (or root) may even speak to it.
                let allowed = stream
                    .peer_cred()
                    .map(|c| c.uid() == rustix::process::geteuid().as_raw() || c.uid() == 0)
                    .unwrap_or(false);
                if !allowed {
                    tracing::warn!("rejected an IPC connection from a different user");
                    continue;
                }
                return Ok(PendingConnection {
                    stream: Box::new(stream),
                    token,
                });
            },
            #[cfg(windows)]
            Listener::Pipe { name, next } => {
                use tokio::net::windows::named_pipe::ServerOptions;
                let replacement = ServerOptions::new()
                    .reject_remote_clients(true)
                    .create(name.as_str())?;
                let connected = std::mem::replace(next, replacement);
                connected.connect().await?;
                Ok(PendingConnection {
                    stream: Box::new(connected),
                    token,
                })
            }
        }
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        // Leave nothing behind that points at a dead agent.
        #[cfg(unix)]
        if let IpcEndpoint::Unix(path) = &self.paths.endpoint {
            let _ = std::fs::remove_file(path);
        }
        let _ = std::fs::remove_file(&self.paths.token_file);
    }
}

fn create_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
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

impl PendingConnection {
    /// Requires the client's token as its first frame, then answers with `welcome`. Returns the
    /// authenticated connection and the client's self-reported name.
    pub async fn authenticate(
        self,
        welcome: IpcWelcome,
    ) -> Result<(IpcConnection, String), IpcError> {
        let mut conn = IpcConnection::new(self.stream);
        let first = timeout(AUTH_TIMEOUT, conn.recv())
            .await
            .map_err(|_| IpcError::Timeout)??;
        let hello = match first {
            Some(IpcFrame {
                body: Some(Body::Hello(h)),
            }) => h,
            _ => return Err(IpcError::Unauthorized),
        };
        let ok: bool = hello.token.as_bytes().ct_eq(self.token.as_bytes()).into();
        if !ok {
            return Err(IpcError::Unauthorized);
        }
        conn.send(&IpcFrame {
            body: Some(Body::Welcome(welcome)),
        })
        .await?;
        Ok((conn, hello.client))
    }
}
