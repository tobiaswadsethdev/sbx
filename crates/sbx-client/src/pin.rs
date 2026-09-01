//! Verifying a server by its certificate's fingerprint, and by nothing else.
//!
//! There is no certificate authority to appeal to here -- the server signed its
//! own certificate -- so the usual chain verification has nothing to check
//! against. What replaces it is the SHA-256 in the pairing string: a
//! certificate is the right one if and only if it hashes to what was pasted.
//!
//! **The hostname is deliberately not checked.** A name check answers "is this
//! the host I dialled", which is a weaker question than "is this the exact
//! certificate I was given", and the fingerprint has already answered the
//! stronger one. Insisting on both would only mean a server reached by an
//! address that was not in its certificate -- a WSL box on an address that
//! changed, a port forward, an SSH tunnel to `localhost` -- fails for a reason
//! that has nothing to do with its identity.
//!
//! What this is *not* is `disable_verification`. Accepting any certificate
//! leaves a TLS session with no idea who is on the other end; this accepts
//! exactly one.

use std::sync::Arc;

use rustls::ClientConfig;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error, SignatureScheme};
use sha2::{Digest, Sha256};

#[derive(Debug)]
pub struct Pinned {
    fingerprint: String,
    provider: Arc<CryptoProvider>,
}

impl Pinned {
    pub fn new(fingerprint: &str, provider: Arc<CryptoProvider>) -> Self {
        Self {
            fingerprint: fingerprint.to_ascii_lowercase(),
            provider,
        }
    }
}

/// A TLS configuration that trusts exactly one certificate.
///
/// Built here rather than at each call site, so the request path and the
/// websocket path cannot end up trusting differently -- which would be a
/// connection that verifies for a policy fetch and not for a terminal, or
/// worse, the other way round.
pub fn client_config(fingerprint: &str) -> Result<ClientConfig, Error> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Ok(ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned::new(fingerprint, provider)))
        .with_no_client_auth())
}

/// Lowercase hex SHA-256, the one form a fingerprint takes anywhere in this
/// codebase.
pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        // Only the leaf. Anything it might chain to is irrelevant: the question
        // is whether *this* certificate is the one that was paired with.
        let presented = fingerprint(end_entity);
        if presented == self.fingerprint {
            return Ok(ServerCertVerified::assertion());
        }
        // Both are named, because the interesting case is a server that was
        // rebuilt and generated a new certificate -- which is indistinguishable
        // from an interception until you compare the two by eye.
        Err(Error::General(format!(
            "the server presented certificate {presented}, and this connection \
             was paired with {}. If the server was rebuilt, pair again; \
             otherwise something is answering in its place.",
            self.fingerprint
        )))
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> Arc<CryptoProvider> {
        Arc::new(rustls::crypto::ring::default_provider())
    }

    /// The certificate bytes are stand-ins: nothing here parses them, which is
    /// the point -- a fingerprint is over the DER, whatever the DER says.
    #[test]
    fn the_paired_certificate_is_accepted_and_no_other_is() {
        let cert = CertificateDer::from(vec![1, 2, 3, 4]);
        let other = CertificateDer::from(vec![1, 2, 3, 5]);

        let verifier = Pinned::new(&fingerprint(&cert), provider());
        let name = ServerName::try_from("localhost").unwrap();
        let now = UnixTime::since_unix_epoch(std::time::Duration::from_secs(1_700_000_000));

        assert!(
            verifier
                .verify_server_cert(&cert, &[], &name, &[], now)
                .is_ok()
        );
        assert!(
            verifier
                .verify_server_cert(&other, &[], &name, &[], now)
                .is_err()
        );
    }

    /// The hostname is not part of the decision, and a test is the only place
    /// that stays true after somebody "fixes" the missing name check.
    #[test]
    fn the_name_dialled_does_not_change_the_answer() {
        let cert = CertificateDer::from(vec![9, 9, 9]);
        let verifier = Pinned::new(&fingerprint(&cert), provider());
        let now = UnixTime::since_unix_epoch(std::time::Duration::from_secs(1_700_000_000));

        for name in ["localhost", "some-box.lan", "10.1.2.3"] {
            let name = ServerName::try_from(name).unwrap();
            assert!(
                verifier
                    .verify_server_cert(&cert, &[], &name, &[], now)
                    .is_ok(),
                "{name:?} was rejected"
            );
        }
    }

    /// The error has to say both fingerprints, because a rebuilt server and an
    /// impostor look identical until you compare them.
    #[test]
    fn a_mismatch_names_what_was_expected_and_what_arrived() {
        let cert = CertificateDer::from(vec![1]);
        let paired = fingerprint(&CertificateDer::from(vec![2]));
        let verifier = Pinned::new(&paired, provider());
        let name = ServerName::try_from("localhost").unwrap();
        let now = UnixTime::since_unix_epoch(std::time::Duration::from_secs(1_700_000_000));

        let err = verifier
            .verify_server_cert(&cert, &[], &name, &[], now)
            .unwrap_err()
            .to_string();
        assert!(err.contains(&paired), "{err}");
        assert!(err.contains(&fingerprint(&cert)), "{err}");
        assert!(err.contains("pair again"), "{err}");
    }

    /// An uppercase fingerprint out of some other tool must not silently fail
    /// to match. `Pairing` lowercases too; this is the second half of that.
    #[test]
    fn a_fingerprint_is_compared_case_insensitively() {
        let cert = CertificateDer::from(vec![7, 7]);
        let verifier = Pinned::new(&fingerprint(&cert).to_ascii_uppercase(), provider());
        let name = ServerName::try_from("localhost").unwrap();
        let now = UnixTime::since_unix_epoch(std::time::Duration::from_secs(1_700_000_000));
        assert!(
            verifier
                .verify_server_cert(&cert, &[], &name, &[], now)
                .is_ok()
        );
    }
}
