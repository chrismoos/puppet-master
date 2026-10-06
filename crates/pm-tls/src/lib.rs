//! Pinned mutual TLS for the worker plane.
//!
//! Both the controller and each host hold one long-lived keypair, and each
//! side pins the other's public key. Certificates are self-signed and carry
//! no useful name or validity claim: identity is the SHA-256 of the peer's
//! SubjectPublicKeyInfo, so a certificate can be regenerated from a stored
//! key without invalidating a pin. Trust is the pin and nothing else.

pub mod pairing;

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{
    ClientConfig, DigitallySignedStruct, DistinguishedName, ServerConfig, SignatureScheme,
};
use sha2::{Digest, Sha256};

/// Certificates are never validated by name, but rustls requires the client
/// to name the server it believes it is reaching.
const PEER_NAME: &str = "pm.peer";

#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("generating key: {0}")]
    KeyGen(String),
    #[error("reading key: {0}")]
    KeyParse(String),
    #[error("reading certificate: {0}")]
    CertParse(String),
    #[error("building TLS config: {0}")]
    Config(String),
}

/// SHA-256 of a SubjectPublicKeyInfo, the stable identity of one peer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyHash([u8; 32]);

impl KeyHash {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn from_hex(text: &str) -> Option<Self> {
        let bytes = hex::decode(text).ok()?;
        Some(Self(bytes.try_into().ok()?))
    }
}

impl std::fmt::Display for KeyHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

fn spki_hash(cert: &CertificateDer<'_>) -> Result<KeyHash, TlsError> {
    let (_, parsed) = x509_parser::parse_x509_certificate(cert.as_ref())
        .map_err(|e| TlsError::CertParse(e.to_string()))?;
    let spki = parsed.tbs_certificate.subject_pki.raw;
    Ok(KeyHash(Sha256::digest(spki).into()))
}

/// This side's long-lived keypair and the self-signed certificate it
/// presents. Only the key is persisted; the certificate is rebuilt from it
/// at startup, which is safe precisely because pinning ignores everything
/// about the certificate except the key it carries.
pub struct Identity {
    cert: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
    key_hash: KeyHash,
}

impl Clone for Identity {
    fn clone(&self) -> Self {
        Self {
            cert: self.cert.clone(),
            key: self.key.clone_key(),
            key_hash: self.key_hash,
        }
    }
}

impl Identity {
    pub fn generate() -> Result<Self, TlsError> {
        let keypair = rcgen::KeyPair::generate().map_err(|e| TlsError::KeyGen(e.to_string()))?;
        Self::from_keypair(keypair)
    }

    pub fn from_key_pem(pem: &str) -> Result<Self, TlsError> {
        let keypair =
            rcgen::KeyPair::from_pem(pem).map_err(|e| TlsError::KeyParse(e.to_string()))?;
        Self::from_keypair(keypair)
    }

    fn from_keypair(keypair: rcgen::KeyPair) -> Result<Self, TlsError> {
        let key = PrivateKeyDer::try_from(keypair.serialize_der())
            .map_err(|e| TlsError::KeyParse(e.to_string()))?;
        let mut params = rcgen::CertificateParams::new(vec![PEER_NAME.to_string()])
            .map_err(|e| TlsError::KeyGen(e.to_string()))?;
        params.distinguished_name = rcgen::DistinguishedName::new();
        let cert = params
            .self_signed(&keypair)
            .map_err(|e| TlsError::KeyGen(e.to_string()))?;
        let cert = CertificateDer::from(cert.der().to_vec());
        let key_hash = spki_hash(&cert)?;
        Ok(Self {
            cert,
            key,
            key_hash,
        })
    }

    pub fn key_pem(&self) -> Result<String, TlsError> {
        // rcgen keeps no handle to the parsed key, so re-derive the PEM from
        // the DER we hold.
        let keypair = rcgen::KeyPair::try_from(self.key.secret_der())
            .map_err(|e| TlsError::KeyParse(e.to_string()))?;
        Ok(keypair.serialize_pem())
    }

    pub fn key_hash(&self) -> KeyHash {
        self.key_hash
    }
}

/// What the verifier will accept from the peer.
#[derive(Clone)]
pub enum PeerPolicy {
    /// Steady state: exactly this key, nothing else.
    Pinned(KeyHash),
    /// Any one of these keys. A host enrolled with more than one controller
    /// cannot know which is dialing until the handshake names it.
    PinnedAny(Vec<KeyHash>),
    /// First contact only. Any key completes the handshake, but the caller
    /// must run the pairing proof before treating the peer as authentic.
    Pairing,
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// The key this end pinned is not the key the peer presented.
#[derive(Debug, thiserror::Error)]
#[error("peer public key does not match the pinned key")]
pub struct RefusedKey;

fn refused_key_error() -> rustls::Error {
    rustls::Error::InvalidCertificate(rustls::CertificateError::Other(rustls::OtherError(
        Arc::new(RefusedKey),
    )))
}

/// True when a handshake failed because one end refused the other's key,
/// rather than because the connection never got that far. Only the first
/// says anything about who the peer is: a refused connection during a
/// restart says nothing at all, and acting on it as though the peer had
/// changed would discard an enrollment that still works.
pub fn refused_key(error: &std::io::Error) -> bool {
    error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
        .is_some_and(is_refused_key)
}

fn is_refused_key(error: &rustls::Error) -> bool {
    match error {
        rustls::Error::InvalidCertificate(rustls::CertificateError::Other(other)) => {
            other.0.downcast_ref::<RefusedKey>().is_some()
        }
        _ => false,
    }
}

#[derive(Debug)]
struct PinnedPeer {
    /// None while pairing, when there is no pin to check yet.
    pinned: Option<Vec<KeyHash>>,
    provider: Arc<CryptoProvider>,
}

impl PinnedPeer {
    fn new(policy: &PeerPolicy, provider: Arc<CryptoProvider>) -> Self {
        Self {
            pinned: match policy {
                PeerPolicy::Pinned(hash) => Some(vec![*hash]),
                PeerPolicy::PinnedAny(hashes) => Some(hashes.clone()),
                PeerPolicy::Pairing => None,
            },
            provider,
        }
    }

    fn check(&self, end_entity: &CertificateDer<'_>) -> Result<(), rustls::Error> {
        let Some(expected) = self.pinned.as_deref() else {
            return Ok(());
        };
        let presented = spki_hash(end_entity)
            .map_err(|e| rustls::Error::General(format!("peer certificate: {e}")))?;
        if expected.contains(&presented) {
            Ok(())
        } else {
            Err(refused_key_error())
        }
    }

    fn schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

impl ServerCertVerifier for PinnedPeer {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        self.check(end_entity)?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
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
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes()
    }
}

impl ClientCertVerifier for PinnedPeer {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        self.check(end_entity)?;
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
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
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes()
    }
}

/// The name a dialer must pass to rustls. Verification ignores it.
pub fn peer_server_name() -> ServerName<'static> {
    ServerName::try_from(PEER_NAME).expect("static peer name is a valid DNS name")
}

pub fn client_config(
    identity: &Identity,
    policy: &PeerPolicy,
) -> Result<Arc<ClientConfig>, TlsError> {
    let provider = provider();
    let verifier = Arc::new(PinnedPeer::new(policy, provider.clone()));
    let config = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| TlsError::Config(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![identity.cert.clone()], identity.key.clone_key())
        .map_err(|e| TlsError::Config(e.to_string()))?;
    Ok(Arc::new(config))
}

pub fn server_config(
    identity: &Identity,
    policy: &PeerPolicy,
) -> Result<Arc<ServerConfig>, TlsError> {
    let provider = provider();
    let verifier = Arc::new(PinnedPeer::new(policy, provider.clone()));
    let config = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| TlsError::Config(e.to_string()))?
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![identity.cert.clone()], identity.key.clone_key())
        .map_err(|e| TlsError::Config(e.to_string()))?;
    Ok(Arc::new(config))
}

/// Ordinary server TLS for the browser plane: a certificate chain and key
/// the operator supplies, which browsers check against their own roots.
/// Nothing here pins anything, so this config must never be used for the
/// worker plane.
pub fn web_server_config(
    cert_chain_pem: &[u8],
    key_pem: &[u8],
) -> Result<Arc<ServerConfig>, TlsError> {
    let certs = CertificateDer::pem_slice_iter(cert_chain_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| TlsError::CertParse(e.to_string()))?;
    if certs.is_empty() {
        return Err(TlsError::CertParse(
            "no certificate found in PEM input".to_string(),
        ));
    }
    let key =
        PrivateKeyDer::from_pem_slice(key_pem).map_err(|e| TlsError::KeyParse(e.to_string()))?;
    let mut config = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| TlsError::Config(e.to_string()))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| TlsError::Config(e.to_string()))?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// The key a PEM certificate chain's first certificate carries, which is
/// what a client pinning the web listener compares against.
pub fn web_cert_key_hash(cert_chain_pem: &[u8]) -> Result<KeyHash, TlsError> {
    let leaf = CertificateDer::pem_slice_iter(cert_chain_pem)
        .next()
        .ok_or_else(|| TlsError::CertParse("no certificate found in PEM input".to_string()))?
        .map_err(|e| TlsError::CertParse(e.to_string()))?;
    spki_hash(&leaf)
}

/// How a client of the web listener decides it reached the controller it
/// means to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebTrust {
    /// The certificate must chain to a root the operating system trusts.
    SystemRoots,
    /// The certificate must carry exactly this key. For a controller whose
    /// certificate no system root vouches for.
    Pinned(KeyHash),
    /// Any certificate completes the handshake. Only for reading the key a
    /// server presents so a person can decide whether to pin it: nothing
    /// secret may be sent over a connection made this way.
    ReadKeyOnly,
}

/// A client config for the web listener, without client authentication.
pub fn web_client_config(trust: &WebTrust) -> Result<ClientConfig, TlsError> {
    let provider = provider();
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| TlsError::Config(e.to_string()))?;
    let policy = match trust {
        WebTrust::SystemRoots => {
            let mut roots = rustls::RootCertStore::empty();
            let loaded = rustls_native_certs::load_native_certs();
            roots.add_parsable_certificates(loaded.certs);
            if roots.is_empty() {
                return Err(TlsError::Config(
                    "the operating system offered no trusted root certificates".to_string(),
                ));
            }
            return Ok(builder.with_root_certificates(roots).with_no_client_auth());
        }
        WebTrust::Pinned(hash) => PeerPolicy::Pinned(*hash),
        WebTrust::ReadKeyOnly => PeerPolicy::Pairing,
    };
    Ok(builder
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedPeer::new(&policy, provider)))
        .with_no_client_auth())
}

/// A self-signed certificate and key for the given names, as the PEM pair
/// `web_server_config` reads. For local testing of the TLS listener, not a
/// substitute for a certificate browsers trust.
pub fn self_signed_web_cert_pem(names: &[String]) -> Result<(String, String), TlsError> {
    let keypair = rcgen::KeyPair::generate().map_err(|e| TlsError::KeyGen(e.to_string()))?;
    let cert = rcgen::CertificateParams::new(names.to_vec())
        .map_err(|e| TlsError::KeyGen(e.to_string()))?
        .self_signed(&keypair)
        .map_err(|e| TlsError::KeyGen(e.to_string()))?;
    Ok((cert.pem(), keypair.serialize_pem()))
}

/// The peer's identity as proven by the completed handshake.
pub fn peer_key_hash(certs: Option<&[CertificateDer<'_>]>) -> Option<KeyHash> {
    spki_hash(certs?.first()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_regenerated_certificate_keeps_the_same_identity() {
        let first = Identity::generate().unwrap();
        let pem = first.key_pem().unwrap();
        let second = Identity::from_key_pem(&pem).unwrap();
        assert_eq!(first.key_hash(), second.key_hash());
    }

    fn web_handshake(server: Arc<ServerConfig>, trust: &WebTrust) -> Result<(), rustls::Error> {
        let name = ServerName::try_from("pm.example").unwrap();
        let mut client =
            rustls::ClientConnection::new(Arc::new(web_client_config(trust).unwrap()), name)
                .unwrap();
        let mut server = rustls::ServerConnection::new(server).unwrap();
        while client.is_handshaking() || server.is_handshaking() {
            let mut wire = Vec::new();
            while client.wants_write() {
                client.write_tls(&mut wire).unwrap();
            }
            if !wire.is_empty() {
                server.read_tls(&mut wire.as_slice()).unwrap();
                server.process_new_packets()?;
            }
            let mut wire = Vec::new();
            while server.wants_write() {
                server.write_tls(&mut wire).unwrap();
            }
            if !wire.is_empty() {
                client.read_tls(&mut wire.as_slice()).unwrap();
                client.process_new_packets()?;
            }
        }
        Ok(())
    }

    fn web_server_and_key() -> (Arc<ServerConfig>, KeyHash) {
        let (cert, key) = self_signed_web_cert_pem(&["pm.example".to_string()]).unwrap();
        let hash = web_cert_key_hash(cert.as_bytes()).unwrap();
        (
            web_server_config(cert.as_bytes(), key.as_bytes()).unwrap(),
            hash,
        )
    }

    #[test]
    fn a_pinned_web_client_accepts_only_the_pinned_key() {
        let (server, key) = web_server_and_key();
        web_handshake(server.clone(), &WebTrust::Pinned(key)).unwrap();
        let (_, other) = web_server_and_key();
        let refused = web_handshake(server, &WebTrust::Pinned(other)).unwrap_err();
        assert!(is_refused_key(&refused), "{refused:?}");
    }

    #[test]
    fn a_self_signed_web_certificate_is_refused_under_system_roots() {
        let (server, _) = web_server_and_key();
        let Ok(_) = web_client_config(&WebTrust::SystemRoots) else {
            return;
        };
        assert!(matches!(
            web_handshake(server, &WebTrust::SystemRoots),
            Err(rustls::Error::InvalidCertificate(_))
        ));
    }

    #[test]
    fn a_web_server_config_reads_a_pem_chain_and_key() {
        let (cert, key) = self_signed_web_cert_pem(&["pm.example".to_string()]).unwrap();
        let config = web_server_config(cert.as_bytes(), key.as_bytes()).unwrap();
        assert_eq!(
            config.alpn_protocols,
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        );
    }

    #[test]
    fn a_web_server_config_rejects_a_key_that_does_not_match_the_certificate() {
        let (cert, _) = self_signed_web_cert_pem(&["pm.example".to_string()]).unwrap();
        let (_, other_key) = self_signed_web_cert_pem(&["pm.example".to_string()]).unwrap();
        assert!(matches!(
            web_server_config(cert.as_bytes(), other_key.as_bytes()),
            Err(TlsError::Config(_))
        ));
    }

    #[test]
    fn a_web_server_config_names_what_is_missing() {
        let (cert, key) = self_signed_web_cert_pem(&["pm.example".to_string()]).unwrap();
        assert!(matches!(
            web_server_config(b"not a certificate", key.as_bytes()),
            Err(TlsError::CertParse(_))
        ));
        assert!(matches!(
            web_server_config(cert.as_bytes(), b"not a key"),
            Err(TlsError::KeyParse(_))
        ));
    }

    #[test]
    fn distinct_keys_have_distinct_identities() {
        let a = Identity::generate().unwrap();
        let b = Identity::generate().unwrap();
        assert_ne!(a.key_hash(), b.key_hash());
    }

    #[test]
    fn key_hashes_round_trip_through_hex() {
        let identity = Identity::generate().unwrap();
        let hash = identity.key_hash();
        assert_eq!(KeyHash::from_hex(&hash.to_hex()), Some(hash));
        assert_eq!(KeyHash::from_hex("not hex"), None);
        assert_eq!(KeyHash::from_hex("aabb"), None);
    }

    #[test]
    fn the_pin_accepts_only_the_pinned_key() {
        let pinned = Identity::generate().unwrap();
        let other = Identity::generate().unwrap();
        let verifier = PinnedPeer::new(&PeerPolicy::Pinned(pinned.key_hash()), provider());
        assert!(verifier.check(&pinned.cert).is_ok());
        assert!(verifier.check(&other.cert).is_err());
    }

    /// A host enrolled with several controllers cannot know which is dialing
    /// until the handshake names it, so any of its pins may answer.
    #[test]
    fn any_of_several_pins_is_accepted_and_nothing_else() {
        let first = Identity::generate().unwrap();
        let second = Identity::generate().unwrap();
        let stranger = Identity::generate().unwrap();
        let verifier = PinnedPeer::new(
            &PeerPolicy::PinnedAny(vec![first.key_hash(), second.key_hash()]),
            provider(),
        );
        assert!(verifier.check(&first.cert).is_ok());
        assert!(verifier.check(&second.cert).is_ok());
        assert!(verifier.check(&stranger.cert).is_err());
        assert!(
            PinnedPeer::new(&PeerPolicy::PinnedAny(Vec::new()), provider())
                .check(&first.cert)
                .is_err()
        );
    }

    #[test]
    fn a_pairing_verifier_accepts_an_unknown_key() {
        let unknown = Identity::generate().unwrap();
        let verifier = PinnedPeer::new(&PeerPolicy::Pairing, provider());
        assert!(verifier.check(&unknown.cert).is_ok());
    }

    #[test]
    fn configs_build_for_both_roles() {
        let identity = Identity::generate().unwrap();
        let policy = PeerPolicy::Pinned(identity.key_hash());
        assert!(client_config(&identity, &policy).is_ok());
        assert!(server_config(&identity, &policy).is_ok());
        let _ = peer_server_name();
    }

    /// Drives a handshake between two in-memory connections. Returns the
    /// verification error from whichever side rejected its peer.
    fn handshake(
        client: &mut rustls::ClientConnection,
        server: &mut rustls::ServerConnection,
    ) -> Result<(), rustls::Error> {
        for _ in 0..8 {
            if !client.is_handshaking() && !server.is_handshaking() {
                break;
            }
            let mut buf = Vec::new();
            while client.wants_write() {
                client.write_tls(&mut buf).unwrap();
            }
            if !buf.is_empty() {
                server.read_tls(&mut buf.as_slice()).unwrap();
                server.process_new_packets()?;
            }
            let mut buf = Vec::new();
            while server.wants_write() {
                server.write_tls(&mut buf).unwrap();
            }
            if !buf.is_empty() {
                client.read_tls(&mut buf.as_slice()).unwrap();
                client.process_new_packets()?;
            }
        }
        Ok(())
    }

    struct Peers {
        client: rustls::ClientConnection,
        server: rustls::ServerConnection,
    }

    fn peers(
        dialer: &Identity,
        listener: &Identity,
        on_dialer: PeerPolicy,
        on_listener: PeerPolicy,
    ) -> Peers {
        Peers {
            client: rustls::ClientConnection::new(
                client_config(dialer, &on_dialer).unwrap(),
                peer_server_name(),
            )
            .unwrap(),
            server: rustls::ServerConnection::new(server_config(listener, &on_listener).unwrap())
                .unwrap(),
        }
    }

    #[test]
    fn two_pinned_peers_complete_a_handshake_and_learn_each_other() {
        let dialer = Identity::generate().unwrap();
        let listener = Identity::generate().unwrap();
        let mut p = peers(
            &dialer,
            &listener,
            PeerPolicy::Pinned(listener.key_hash()),
            PeerPolicy::Pinned(dialer.key_hash()),
        );
        handshake(&mut p.client, &mut p.server).unwrap();
        assert!(!p.client.is_handshaking());
        assert_eq!(
            peer_key_hash(p.client.peer_certificates()),
            Some(listener.key_hash())
        );
        assert_eq!(
            peer_key_hash(p.server.peer_certificates()),
            Some(dialer.key_hash())
        );
    }

    #[test]
    fn a_listener_refuses_a_dialer_it_has_not_pinned() {
        let dialer = Identity::generate().unwrap();
        let listener = Identity::generate().unwrap();
        let impostor = Identity::generate().unwrap();
        let mut p = peers(
            &dialer,
            &listener,
            PeerPolicy::Pinned(listener.key_hash()),
            PeerPolicy::Pinned(impostor.key_hash()),
        );
        let error = handshake(&mut p.client, &mut p.server).unwrap_err();
        assert!(is_refused_key(&error), "unexpected error: {error}");
    }

    #[test]
    fn a_dialer_refuses_a_listener_it_has_not_pinned() {
        let dialer = Identity::generate().unwrap();
        let listener = Identity::generate().unwrap();
        let impostor = Identity::generate().unwrap();
        let mut p = peers(
            &dialer,
            &listener,
            PeerPolicy::Pinned(impostor.key_hash()),
            PeerPolicy::Pinned(dialer.key_hash()),
        );
        let error = handshake(&mut p.client, &mut p.server).unwrap_err();
        assert!(is_refused_key(&error), "unexpected error: {error}");
    }

    /// The pairing window is the one time an unpinned key gets this far, and
    /// it buys the peer nothing on its own — the caller still runs the proof.
    #[test]
    fn pairing_admits_an_unpinned_peer_and_reports_its_key() {
        let dialer = Identity::generate().unwrap();
        let listener = Identity::generate().unwrap();
        let mut p = peers(&dialer, &listener, PeerPolicy::Pairing, PeerPolicy::Pairing);
        handshake(&mut p.client, &mut p.server).unwrap();
        assert_eq!(
            peer_key_hash(p.server.peer_certificates()),
            Some(dialer.key_hash())
        );
    }

    /// Both ends of one session export the same secret, and a second session
    /// between the same two keys exports a different one. That difference is
    /// what stops a relayed pairing proof.
    #[test]
    fn exported_material_matches_per_session_and_differs_across_them() {
        let dialer = Identity::generate().unwrap();
        let listener = Identity::generate().unwrap();
        let export = |p: &mut Peers| {
            handshake(&mut p.client, &mut p.server).unwrap();
            (
                crate::pairing::exporter(&p.client).unwrap(),
                crate::pairing::exporter(&p.server).unwrap(),
            )
        };
        let mut first = peers(
            &dialer,
            &listener,
            PeerPolicy::Pinned(listener.key_hash()),
            PeerPolicy::Pinned(dialer.key_hash()),
        );
        let (client_first, server_first) = export(&mut first);
        assert_eq!(client_first, server_first);

        let mut second = peers(
            &dialer,
            &listener,
            PeerPolicy::Pinned(listener.key_hash()),
            PeerPolicy::Pinned(dialer.key_hash()),
        );
        let (client_second, _) = export(&mut second);
        assert_ne!(client_first, client_second);
    }
}
