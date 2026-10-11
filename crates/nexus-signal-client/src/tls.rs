use std::sync::Arc;

use rustls::ClientConfig;
use rustls_platform_verifier::BuilderVerifierExt;
use tokio_tungstenite::Connector;

use crate::error::ClientError;

/// TLS 1.2+ (servers should only offer 1.3) using the operating system's trust store and
/// verifier, with the `ring` crypto provider (no C toolchain needed to build).
pub(crate) fn connector() -> Result<Connector, ClientError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| ClientError::Tls(e.to_string()))?
        .with_platform_verifier()
        .map_err(|e| ClientError::Tls(e.to_string()))?
        .with_no_client_auth();
    Ok(Connector::Rustls(Arc::new(config)))
}
