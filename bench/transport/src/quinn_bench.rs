//! Raw quinn over loopback with a self-signed certificate (verification skipped: this is a
//! stack-overhead measurement, not a security configuration).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use nexus_core::clock::now_us;
use nexus_core::telemetry::LatencyStats;
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

use crate::Outcome;

#[derive(Debug)]
struct SkipVerify(Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for SkipVerify {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

pub async fn run(pings: usize, bulk_mb: usize) -> anyhow::Result<Outcome> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
    let cert_der = CertificateDer::from(cert.cert.der().to_vec());
    let key = PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());

    let mut server_tls = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key.into())?;
    server_tls.alpn_protocols = vec![b"bench".to_vec()];
    let mut server_cfg = quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(server_tls)?));
    let mut transport = quinn::TransportConfig::default();
    transport.datagram_receive_buffer_size(Some(1 << 20)).datagram_send_buffer_size(1 << 20);
    server_cfg.transport_config(Arc::new(transport));
    let server = quinn::Endpoint::server(server_cfg, "127.0.0.1:0".parse::<SocketAddr>()?)?;
    let addr = server.local_addr()?;

    // Server: echo streams (mode 0) / count bulk (mode 1) / echo datagrams.
    tokio::spawn(async move {
        while let Some(incoming) = server.accept().await {
            let Ok(conn) = incoming.await else { continue };
            let dg = conn.clone();
            tokio::spawn(async move {
                while let Ok(d) = dg.read_datagram().await {
                    if dg.send_datagram(d).is_err() {
                        break;
                    }
                }
            });
            tokio::spawn(async move {
                while let Ok((mut send, mut recv)) = conn.accept_bi().await {
                    tokio::spawn(async move {
                        let mut mode = [0u8; 1];
                        if recv.read_exact(&mut mode).await.is_err() {
                            return;
                        }
                        let mut buf = vec![0u8; 64 * 1024];
                        if mode[0] == 0 {
                            while let Ok(Some(n)) = recv.read(&mut buf).await {
                                if send.write_all(&buf[..n]).await.is_err() {
                                    return;
                                }
                            }
                            let _ = send.finish();
                        } else {
                            let mut total = 0u64;
                            while let Ok(Some(n)) = recv.read(&mut buf).await {
                                total += n as u64;
                            }
                            let _ = send.write_all(&total.to_le_bytes()).await;
                            let _ = send.finish();
                        }
                    });
                }
            });
        }
    });

    let mut client_tls = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipVerify(provider)))
        .with_no_client_auth();
    client_tls.alpn_protocols = vec![b"bench".to_vec()];
    let mut client_cfg = quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(client_tls)?));
    let mut transport = quinn::TransportConfig::default();
    transport.datagram_receive_buffer_size(Some(1 << 20)).datagram_send_buffer_size(1 << 20);
    client_cfg.transport_config(Arc::new(transport));
    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse::<SocketAddr>()?)?;
    endpoint.set_default_client_config(client_cfg);
    let conn = endpoint.connect(addr, "localhost")?.await?;

    // Reliable small echoes.
    let (mut send, mut recv) = conn.open_bi().await?;
    send.write_all(&[0]).await?;
    let mut reliable = LatencyStats::new();
    let mut reply = [0u8; 64];
    for _ in 0..pings {
        let t = now_us();
        let mut msg = [0u8; 64];
        msg[..8].copy_from_slice(&t.to_le_bytes());
        send.write_all(&msg).await?;
        recv.read_exact(&mut reply).await?;
        reliable.record_us(now_us().saturating_sub(t));
    }
    let _ = send.finish();

    // Unreliable datagram echoes.
    let mut unreliable = LatencyStats::new();
    let rx = conn.clone();
    let collector = tokio::spawn(async move {
        let mut got = Vec::new();
        while got.len() < pings {
            match tokio::time::timeout(std::time::Duration::from_secs(1), rx.read_datagram()).await {
                Ok(Ok(d)) if d.len() >= 8 => {
                    let mut ts = [0u8; 8];
                    ts.copy_from_slice(&d[..8]);
                    got.push(now_us().saturating_sub(u64::from_le_bytes(ts)));
                }
                _ => break,
            }
        }
        got
    });
    for _ in 0..pings {
        let mut d = vec![0u8; 100];
        d[..8].copy_from_slice(&now_us().to_le_bytes());
        conn.send_datagram(Bytes::from(d))?;
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    let got = collector.await?;
    let loss = 1.0 - got.len() as f64 / pings as f64;
    for us in &got {
        unreliable.record_us(*us);
    }

    // Bulk.
    let (mut send, mut recv) = conn.open_bi().await?;
    send.write_all(&[1]).await?;
    let chunk = vec![0xA5u8; 64 * 1024];
    let total = bulk_mb * 1024 * 1024;
    let started = Instant::now();
    let mut left = total;
    while left > 0 {
        let n = left.min(chunk.len());
        send.write_all(&chunk[..n]).await?;
        left -= n;
    }
    send.finish()?;
    let mut count = [0u8; 8];
    recv.read_exact(&mut count).await?;
    anyhow::ensure!(u64::from_le_bytes(count) == total as u64, "byte count mismatch");
    let mbps = total as f64 * 8.0 / started.elapsed().as_secs_f64() / 1e6;

    Ok(Outcome { name: "quinn 0.11 (raw)", reliable: reliable.summary(), unreliable: unreliable.summary(), loss, throughput_mbps: mbps })
}
