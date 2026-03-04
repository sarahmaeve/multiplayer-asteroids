//! Generates a self-signed TLS certificate at startup and builds a
//! [`rustls::ServerConfig`] from it.
//!
//! For production use, replace this with a cert issued by a real CA and load it
//! from disk.  The in-memory self-signed cert here is intentional for a LAN /
//! development context; it still provides full encryption of the traffic.

use std::sync::Arc;

use anyhow::Context;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::ServerConfig;

/// Generate a fresh self-signed certificate and return a [`ServerConfig`].
///
/// The cert covers `localhost` and `127.0.0.1`; extend `sans` for real
/// deployments.
pub fn build_server_config() -> anyhow::Result<Arc<ServerConfig>> {
    let sans = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    let cert = rcgen::generate_simple_self_signed(sans).context("generate self-signed cert")?;

    let cert_der: CertificateDer<'static> =
        CertificateDer::from(cert.serialize_der().context("serialise cert DER")?);
    let key_der: PrivateKeyDer<'static> =
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.serialize_private_key_der()));

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .context("build ServerConfig")?;

    Ok(Arc::new(config))
}
