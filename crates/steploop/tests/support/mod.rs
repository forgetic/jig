//! A test PKI for the client planner's TLS tests: an `rcgen` CA, a leaf it
//! signed for `localhost`, and a rustls server config using it.

#![allow(dead_code)] // each test crate uses a different part

use std::sync::Arc;

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{RootCertStore, ServerConfig};

pub struct Pki {
    /// Trusts the CA that signed `chain`.
    pub roots: RootCertStore,
    /// A leaf for `localhost`.
    pub chain: Vec<CertificateDer<'static>>,
    pub key: PrivateKeyDer<'static>,
}

/// A CA called `ca_name` and a leaf it signed. CAs in different tests get
/// different names: webpki matches issuers by name, so a stranger CA with the
/// same name gives `BadSignature` rather than `UnknownIssuer`.
pub fn pki(ca_name: &str) -> Pki {
    let ca_key = KeyPair::generate().expect("CA key");
    let mut ca = CertificateParams::new(Vec::<String>::new()).expect("CA params");
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.distinguished_name.push(DnType::CommonName, ca_name);
    ca.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let ca_cert = ca.self_signed(&ca_key).expect("CA cert");
    let leaf_key = KeyPair::generate().expect("leaf key");
    let leaf = CertificateParams::new(vec!["localhost".to_string()]).expect("leaf params");
    let leaf_cert = leaf
        .signed_by(&leaf_key, &Issuer::new(ca, ca_key))
        .expect("leaf cert");
    let mut roots = RootCertStore::empty();
    roots
        .add(ca_cert.der().clone())
        .expect("CA is a trust anchor");
    Pki {
        roots,
        chain: vec![leaf_cert.der().clone()],
        key: PrivatePkcs8KeyDer::from(leaf_key.serialize_der()).into(),
    }
}

/// A server that presents the PKI's leaf.
pub fn server_config(pki: &Pki) -> Arc<ServerConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_no_client_auth()
        .with_single_cert(pki.chain.clone(), pki.key.clone_key())
        .expect("server cert");
    Arc::new(config)
}
