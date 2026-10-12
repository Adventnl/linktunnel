//! A standalone relay. End-to-end QUIC encryption remains between devices.
#![forbid(unsafe_code)]

use anyhow::{Context, bail};
use clap::Parser;
use iroh_relay::server::{
    Access, AccessControl, CertConfig, ClientRateLimit, ClientRequest, RelayConfig, Server,
    ServerConfig, TlsConfig,
};
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use subtle::ConstantTimeEq;

#[derive(Parser)]
#[command(about = "Nexus encrypted relay server")]
struct Args {
    /// HTTP probe / reverse-proxy listener. Without TLS, bind loopback only.
    #[arg(long, default_value = "127.0.0.1:3340")]
    listen: SocketAddr,
    /// Direct HTTPS listener (requires --cert and --key).
    #[arg(long, requires_all = ["cert", "key"])]
    tls_listen: Option<SocketAddr>,
    /// Optional QUIC address-discovery listener (UDP, requires direct TLS).
    #[arg(long, requires = "tls_listen")]
    quic_listen: Option<SocketAddr>,
    /// PEM certificate chain.
    #[arg(long, requires = "tls_listen")]
    cert: Option<PathBuf>,
    /// PEM private key.
    #[arg(long, requires = "tls_listen")]
    key: Option<PathBuf>,
    /// Shared relay admission token. Set via environment to avoid command-line exposure.
    #[arg(long, env = "NEXUS_RELAY_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// Deliberately operate a public relay; otherwise a token is required.
    #[arg(long, conflicts_with = "token")]
    public: bool,
    /// Per-client ingress bytes/second (12.5 MB/s = 100 Mbit/s).
    #[arg(long, default_value = "12500000")]
    bytes_per_second: NonZeroU32,
}

struct Admission {
    token: Option<String>,
}
impl std::fmt::Debug for Admission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Admission(<redacted>)")
    }
}
impl AccessControl for Admission {
    async fn on_connect(&self, request: &ClientRequest) -> Access {
        match &self.token {
            None => Access::Allow,
            Some(expected)
                if request.auth_token().is_some_and(|actual| {
                    bool::from(actual.as_bytes().ct_eq(expected.as_bytes()))
                }) =>
            {
                Access::Allow
            }
            Some(_) => Access::Deny {
                reason: Some("relay authorization required".into()),
            },
        }
    }
}

fn config(args: &Args) -> anyhow::Result<ServerConfig> {
    if args.token.as_ref().is_some_and(|t| t.len() < 32) {
        bail!("relay token must be at least 32 bytes");
    }
    if args.token.is_none() && !args.public {
        bail!("set NEXUS_RELAY_TOKEN or explicitly select --public");
    }
    if args.tls_listen.is_none() && !args.listen.ip().is_loopback() {
        bail!("plain HTTP must bind loopback behind a TLS reverse proxy");
    }
    let mut relay = RelayConfig::new(args.listen);
    relay.key_cache_capacity = Some(4096);
    relay.limits.client_rx = Some(ClientRateLimit::new(args.bytes_per_second));
    relay.access = Arc::new(Admission {
        token: args.token.clone(),
    });
    if let (Some(addr), Some(cert), Some(key)) = (args.tls_listen, &args.cert, &args.key) {
        use rustls_pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
        let certs = CertificateDer::pem_file_iter(cert)
            .context("read certificate chain")?
            .collect::<Result<Vec<_>, _>>()?;
        let key = PrivateKeyDer::from_pem_file(key).context("read PEM private key")?;
        let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
        relay.tls = Some(TlsConfig::new(
            addr,
            CertConfig::Manual { server_config: tls },
        ));
    }
    let mut config = ServerConfig::default();
    config.relay = Some(relay);
    config.quic = args.quic_listen.map(iroh_relay::server::QuicConfig::new);
    Ok(config)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    let mut server = Server::spawn(config(&args)?).await.context("start relay")?;
    tracing::info!(http = ?server.http_addr(), https = ?server.https_addr(), "relay ready");
    tokio::select! {
        result = server.join() => { result.context("relay supervisor")?.context("relay stopped unexpectedly")?; }
        result = tokio::signal::ctrl_c() => { result?; }
    }
    server.shutdown().await.context("stop relay")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closed_by_default_and_cleartext_stays_local() {
        let args = Args::try_parse_from(["relay"]).unwrap();
        assert!(config(&args).is_err());
        let args = Args::try_parse_from(["relay", "--public", "--listen", "0.0.0.0:3340"]).unwrap();
        assert!(config(&args).is_err());
        let args = Args::try_parse_from(["relay", "--public"]).unwrap();
        assert!(config(&args).is_ok());
    }
    #[tokio::test]
    async fn starts_and_shuts_down() {
        let args = Args::try_parse_from(["relay", "--public", "--listen", "127.0.0.1:0"]).unwrap();
        let server = Server::spawn(config(&args).unwrap()).await.unwrap();
        let address = server.http_addr().unwrap();
        assert!(tokio::net::TcpStream::connect(address).await.is_ok());
        server.shutdown().await.unwrap();
    }
}
