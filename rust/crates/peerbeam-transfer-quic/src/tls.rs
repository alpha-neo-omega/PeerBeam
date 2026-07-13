//! Zero-config TLS for QUIC.
//!
//! QUIC mandates TLS, but PeerBeam has no PKI: it authenticates *peers* at the
//! application layer (X25519 mutual auth + TOFU, via `peerbeam-transfer`'s
//! `authenticate`/`SecureLink`). So here TLS provides only the encrypted,
//! integrity-protected pipe — each node presents a fresh self-signed
//! certificate and the client accepts any server certificate. **QUIC alone is
//! encrypted but unauthenticated; identity comes from `SecureLink` on top.**

use std::sync::Arc;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

use peerbeam_domain::error::{DomainError, Result};

/// ALPN token so both ends agree they are speaking PeerBeam over QUIC.
const ALPN: &[u8] = b"peerbeam/1";

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn tls_err(e: impl std::fmt::Display) -> DomainError {
    DomainError::Connection(format!("tls: {e}"))
}

/// Build a quinn server config from a freshly generated self-signed cert.
pub fn server_config() -> Result<quinn::ServerConfig> {
    let cert = rcgen::generate_simple_self_signed(vec!["peerbeam".to_string()])
        .map_err(|e| tls_err(format!("self-signed cert: {e}")))?;
    let cert_der = CertificateDer::from(cert.cert);
    let key = PrivateKeyDer::try_from(cert.key_pair.serialize_der())
        .map_err(|e| tls_err(format!("key: {e}")))?;

    let mut crypto = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(tls_err)?
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key)
        .map_err(tls_err)?;
    crypto.alpn_protocols = vec![ALPN.to_vec()];

    let qsc = QuicServerConfig::try_from(crypto).map_err(tls_err)?;
    Ok(quinn::ServerConfig::with_crypto(Arc::new(qsc)))
}

/// Build a quinn client config that accepts any server certificate.
pub fn client_config() -> Result<quinn::ClientConfig> {
    let mut crypto = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(tls_err)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAny(provider())))
        .with_no_client_auth();
    crypto.alpn_protocols = vec![ALPN.to_vec()];

    let qcc = QuicClientConfig::try_from(crypto).map_err(tls_err)?;
    Ok(quinn::ClientConfig::new(Arc::new(qcc)))
}

/// Accepts any server certificate. Safe here only because peer identity is
/// established by the application-layer handshake, not by TLS.
#[derive(Debug)]
struct AcceptAny(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
