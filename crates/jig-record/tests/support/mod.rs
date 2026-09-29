//! Local upstreams for the recorder's socket tests: an `rcgen` PKI whose leaf
//! is valid for the real providers' hosts (so the routes' own TLS names are
//! the ones checked), a rustls server config, and blocking helpers for
//! upstreams and clients on std threads.

#![allow(dead_code)] // each test crate uses a different part

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{RootCertStore, ServerConfig, ServerConnection, StreamOwned};

pub const TIMEOUT: Duration = Duration::from_secs(30);

/// A TLS upstream's end of one connection.
pub type Tls = StreamOwned<ServerConnection, TcpStream>;

pub struct Pki {
    /// Trusts the CA that signed `chain`.
    pub roots: RootCertStore,
    pub chain: Vec<CertificateDer<'static>>,
    pub key: PrivateKeyDer<'static>,
}

/// A CA called `ca_name` and a leaf it signed for the providers' hosts. CAs
/// in different tests get different names: webpki matches issuers by name, so
/// a stranger CA with the same name gives `BadSignature`, not
/// `UnknownIssuer`.
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
    let hosts = ["api.openai.com", "api.anthropic.com", "chatgpt.com"];
    let leaf_key = KeyPair::generate().expect("leaf key");
    let leaf = CertificateParams::new(hosts.map(String::from).to_vec()).expect("leaf params");
    let leaf_cert = leaf
        .signed_by(&leaf_key, &Issuer::new(ca, ca_key))
        .expect("leaf cert");
    let mut roots = RootCertStore::empty();
    roots.add(ca_cert.der().clone()).expect("a trust anchor");
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

pub fn listen() -> (TcpListener, SocketAddr) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    (l, addr)
}

/// The next connection, with timeouts.
pub fn accept(l: &TcpListener) -> TcpStream {
    let (s, _) = l.accept().unwrap();
    with_timeouts(s)
}

/// The next connection, as a TLS server (the handshake happens on first use).
pub fn accept_tls(l: &TcpListener, config: &Arc<ServerConfig>) -> Tls {
    let conn = ServerConnection::new(Arc::clone(config)).unwrap();
    StreamOwned::new(conn, accept(l))
}

/// Ends a TLS response properly: close_notify, then FIN.
pub fn close(mut tls: Tls) {
    tls.conn.send_close_notify();
    tls.flush().unwrap();
}

pub fn connect(addr: SocketAddr) -> TcpStream {
    with_timeouts(TcpStream::connect(addr).unwrap())
}

fn with_timeouts(s: TcpStream) -> TcpStream {
    s.set_read_timeout(Some(TIMEOUT)).unwrap();
    s.set_write_timeout(Some(TIMEOUT)).unwrap();
    s
}

pub fn addr_of(base_url: &str) -> SocketAddr {
    base_url.strip_prefix("http://").unwrap().parse().unwrap()
}

/// Reads a request: the head, then as many body bytes as its
/// `Content-Length` says.
pub fn read_request(s: &mut impl Read) -> Vec<u8> {
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        if let Some(end) = got.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&got[..end]).to_ascii_lowercase();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .map_or(0, |v| v.trim().parse::<usize>().unwrap());
            if got.len() >= end + 4 + len {
                return got;
            }
        }
        let n = s.read(&mut buf).expect("read the request");
        assert!(n > 0, "the request ended early: {got:?}");
        got.extend_from_slice(&buf[..n]);
    }
}

pub fn post(path: &str, body: &str) -> Vec<u8> {
    format!(
        "POST {path} HTTP/1.1\r\n\
         Host: jig\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    )
    .into_bytes()
}

/// One whole exchange: the raw response, read until the server closes.
pub fn exchange(addr: SocketAddr, request: &[u8]) -> Vec<u8> {
    let mut c = connect(addr);
    c.write_all(request).unwrap();
    let mut got = Vec::new();
    c.read_to_end(&mut got).unwrap();
    got
}

/// What a client gets from a connection closed without a response: nothing,
/// or a reset if some of its bytes were never read.
pub fn assert_closed_without_response(c: &mut TcpStream) {
    let mut got = Vec::new();
    match c.read_to_end(&mut got) {
        Ok(_) => assert_eq!(String::from_utf8_lossy(&got), ""),
        Err(e) => assert_eq!(e.kind(), ErrorKind::ConnectionReset),
    }
}
