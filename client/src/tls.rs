//! Builds a [`rustls::ClientConfig`] that encrypts the connection but does not
//! validate the server certificate.
//!
//! This is intentional for a self-hosted game where the server generates a
//! fresh self-signed cert on every boot.  Traffic is fully encrypted; the only
//! attack that succeeds is an active MITM — acceptable for a LAN game.
//!
//! To harden for public deployment: load the server's certificate fingerprint
//! from a config file and verify it here (Trust-On-First-Use).

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error, SignatureScheme};

/// A certificate verifier that accepts any server certificate.
///
/// Encryption is still active; this only skips identity verification.
#[derive(Debug)]
struct SkipVerification;

impl ServerCertVerifier for SkipVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Build a [`ClientConfig`] with encryption enabled but cert verification
/// delegated to [`SkipVerification`].
pub fn build_client_config() -> Arc<ClientConfig> {
    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipVerification))
        .with_no_client_auth();
    Arc::new(config)
}
