//! HTTP clients that never install or depend on a process-global crypto provider.

use std::sync::Arc;

use rustls_platform_verifier::BuilderVerifierExt as _;

pub fn client_builder() -> Result<reqwest::ClientBuilder, rustls::Error> {
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_platform_verifier()?
    .with_no_client_auth();
    Ok(reqwest::Client::builder().tls_backend_preconfigured(tls))
}

#[cfg(test)]
mod tests {
    #[test]
    fn client_does_not_install_a_global_provider() {
        assert!(rustls::crypto::CryptoProvider::get_default().is_none());
        super::client_builder().unwrap().build().unwrap();
        assert!(rustls::crypto::CryptoProvider::get_default().is_none());
    }
}
