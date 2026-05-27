use crate::SlipstreamError;
use quinn::crypto::rustls::QuicClientConfig;
use rcgen::CertificateParams;
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use solana_sdk::signature::Keypair;
use std::sync::Arc;

pub fn create_quic_config(identity: &Keypair) -> Result<quinn::ClientConfig, SlipstreamError> {
    let rcgen_key = solana_to_rcgen_keypair(identity)?;

    let cert = CertificateParams::new(vec!["solana".to_string()])
        .map_err(|e| SlipstreamError::Certificate(e.to_string()))?
        .self_signed(&rcgen_key)
        .map_err(|e| SlipstreamError::Certificate(e.to_string()))?;

    let cert_chain = vec![CertificateDer::from(cert.der().to_vec())];
    let private_key = PrivatePkcs8KeyDer::from(rcgen_key.serialize_der());

    let mut tls = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipServerVerification::new()))
        .with_client_auth_cert(cert_chain, private_key.into())
        .map_err(|e| SlipstreamError::Certificate(e.to_string()))?;

    tls.alpn_protocols = vec![b"solana-tpu".to_vec()];

    let quic_crypto = QuicClientConfig::try_from(tls)
        .map_err(|e| SlipstreamError::Certificate(format!("quic tls config failed: {e}")))?;
    Ok(quinn::ClientConfig::new(Arc::new(quic_crypto)))
}

#[derive(Debug)]
struct SkipServerVerification(Arc<rustls::crypto::CryptoProvider>);

impl SkipServerVerification {
    fn new() -> Self {
        Self(Arc::new(rustls::crypto::ring::default_provider()))
    }
}

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
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
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn solana_to_rcgen_keypair(solana_keypair: &Keypair) -> Result<rcgen::KeyPair, SlipstreamError> {
    const ED25519_PKCS8_HEADER: &[u8] = &[
        0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04,
        0x20,
    ];

    let full_secret = solana_keypair.secret_bytes();
    let mut pkcs8 = Vec::with_capacity(ED25519_PKCS8_HEADER.len() + 32);
    pkcs8.extend_from_slice(ED25519_PKCS8_HEADER);
    pkcs8.extend_from_slice(&full_secret[0..32]);

    rcgen::KeyPair::try_from(pkcs8.as_slice())
        .map_err(|e| SlipstreamError::Certificate(format!("key conversion failed: {e}")))
}
