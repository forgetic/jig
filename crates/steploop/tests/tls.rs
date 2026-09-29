//! Tests for `steploop::tls`: the client stage against a rustls server, with
//! the test playing the network.
//!
//! [`Net`] shuttles ciphertext between the stage's buffers and a
//! `ServerConnection`, the way a TCP stage and a socket would, cutting it
//! whole, one byte at a time or at random sizes from a seeded PRNG. rustls
//! draws its own randomness, so the bytes differ from run to run but the
//! outcome of every test does not.

#![cfg(feature = "tls")]

use std::io::{self, Read, Write};
use std::sync::Arc;

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use rustls::version::{TLS12, TLS13};
use rustls::{
    ClientConfig, HandshakeKind, ProtocolVersion, RootCertStore, ServerConfig, ServerConnection,
    SupportedProtocolVersion,
};
use steploop::tls::{
    DEFAULT_BUFFER_LIMIT, StageBufs, TlsClient, TlsError, TlsStatus, client_config,
};

// ---------------------------------------------------------------------------
// A test PKI and a server.
// ---------------------------------------------------------------------------

struct Pki {
    /// Trusts the CA that signed `chain`.
    roots: RootCertStore,
    /// A leaf for `localhost` and `api.example.test`.
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
}

/// A CA called `ca_name` and a leaf it signed. CAs in different tests get
/// different names: webpki matches issuers by name, so a stranger CA with the
/// same name gives `BadSignature` rather than `UnknownIssuer`.
fn pki(ca_name: &str) -> Pki {
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
    let leaf = CertificateParams::new(vec![
        "localhost".to_string(),
        "api.example.test".to_string(),
    ])
    .expect("leaf params");
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

/// A server that would rather speak h2, so that negotiating `http/1.1` shows
/// the client offered nothing else.
fn server_config(pki: &Pki, versions: &[&'static SupportedProtocolVersion]) -> Arc<ServerConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(versions)
        .expect("versions")
        .with_no_client_auth()
        .with_single_cert(pki.chain.clone(), pki.key.clone_key())
        .expect("server cert");
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Arc::new(config)
}

fn name(host: &'static str) -> ServerName<'static> {
    ServerName::try_from(host).expect("valid server name")
}

// ---------------------------------------------------------------------------
// The network.
// ---------------------------------------------------------------------------

/// splitmix64: small, and good enough to pick cut sizes.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `1..=max`.
    fn upto(&mut self, max: usize) -> usize {
        1 + (self.next() % max as u64) as usize
    }
}

/// How the network cuts a stream of bytes on each hop.
#[derive(Clone, Copy, Debug)]
enum Cut {
    Whole,
    OneByte,
    Random { max: usize },
}

/// Deterministic, non-repeating-looking payloads, so misordered or lost bytes
/// show up in comparisons.
fn payload(len: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng(seed);
    (0..len).map(|_| rng.next() as u8).collect()
}

struct Net {
    client: TlsClient,
    bufs: StageBufs,
    server: ServerConnection,
    /// Ciphertext the server wrote that has not reached `cipher_in` yet.
    to_client: Vec<u8>,
    /// Plaintext for the server to send.
    server_out: Vec<u8>,
    /// Plaintext the server received.
    server_in: Vec<u8>,
    /// The server received close_notify.
    server_closed: bool,
    server_error: Option<rustls::Error>,
    /// Plaintext the client's reader took from `plain_in`.
    client_in: Vec<u8>,
    status: Option<TlsStatus>,
    error: Option<TlsError>,
    cut: Cut,
    rng: Rng,
    /// Like `tcp::Conn`, stop reading into `cipher_in` at this size.
    inbound_cap: usize,
    /// The client's reader drains `plain_in` every this many rounds (0:
    /// never).
    read_every: usize,
    /// The socket drains `cipher_out`.
    writing: bool,
    /// Checked after every pump: the most `plain_in` and `cipher_out` may
    /// hold once the handshake is over.
    limit: usize,
    rounds: usize,
    /// Bytes moved by the network so far, to tell when nothing moves.
    moved: usize,
    /// Backpressure seen: the stage left ciphertext in `cipher_in` with
    /// `plain_in` full, or plaintext in `plain_out` with `cipher_out` full.
    held_cipher_in: bool,
    held_plain_out: bool,
}

/// What a record adds to its plaintext, with room for a few records and a
/// stray alert: TLS 1.3 adds 22 bytes per record and TLS 1.2 up to 29.
const RECORD_SLACK: usize = 256;

impl Net {
    fn new(
        client_config: Arc<ClientConfig>,
        host: &'static str,
        server_config: Arc<ServerConfig>,
        cut: Cut,
    ) -> Net {
        Net::with_limit(
            client_config,
            host,
            server_config,
            cut,
            DEFAULT_BUFFER_LIMIT,
        )
    }

    fn with_limit(
        client_config: Arc<ClientConfig>,
        host: &'static str,
        server_config: Arc<ServerConfig>,
        cut: Cut,
        limit: usize,
    ) -> Net {
        let client = TlsClient::new(client_config, name(host))
            .expect("client")
            .with_buffer_limit(limit);
        Net {
            client,
            bufs: StageBufs::default(),
            server: ServerConnection::new(server_config).expect("server"),
            to_client: Vec::new(),
            server_out: Vec::new(),
            server_in: Vec::new(),
            server_closed: false,
            server_error: None,
            client_in: Vec::new(),
            status: None,
            error: None,
            cut,
            rng: Rng(0x5eed),
            inbound_cap: usize::MAX,
            read_every: 1,
            writing: true,
            limit,
            rounds: 0,
            moved: 0,
            held_cipher_in: false,
            held_plain_out: false,
        }
    }

    fn cut(&mut self, available: usize) -> usize {
        match self.cut {
            Cut::Whole => available,
            Cut::OneByte => available.min(1),
            Cut::Random { max } => available.min(self.rng.upto(max)),
        }
    }

    fn pump(&mut self) {
        if self.error.is_some() {
            return;
        }
        match self.client.pump(&mut self.bufs) {
            Ok(status) => {
                self.check_invariants(status);
                self.status = Some(status);
            }
            Err(err) => self.error = Some(err),
        }
    }

    fn check_invariants(&mut self, status: TlsStatus) {
        let bufs = &self.bufs;
        assert!(
            bufs.plain_in.len() <= self.limit,
            "plain_in {} over limit {}",
            bufs.plain_in.len(),
            self.limit
        );
        if !status.handshaking {
            assert!(
                bufs.cipher_out.len() <= self.limit + RECORD_SLACK,
                "cipher_out {} over limit {}",
                bufs.cipher_out.len(),
                self.limit
            );
        }
        assert_eq!(status.wants_write, !bufs.cipher_out.is_empty());
        if !bufs.cipher_in.is_empty() && !status.peer_closed {
            // Ciphertext left behind means the reader is the bottleneck.
            assert_eq!(
                bufs.plain_in.len(),
                self.limit,
                "cipher_in held without cause"
            );
            assert!(!status.wants_read);
            self.held_cipher_in = true;
        }
        if !bufs.plain_out.is_empty() && !status.handshaking && !status.close_sent {
            assert!(
                bufs.cipher_out.len() >= self.limit,
                "plain_out held without cause"
            );
            self.held_plain_out = true;
        }
    }

    /// The client's socket sends part of `cipher_out` to the server.
    fn client_to_server(&mut self) {
        if !self.writing {
            return;
        }
        let n = self.cut(self.bufs.cipher_out.len());
        let mut taken = 0;
        while taken < n && self.server_error.is_none() {
            let mut rd = &self.bufs.cipher_out[taken..n];
            match self.server.read_tls(&mut rd) {
                Ok(0) => break,
                Ok(k) => taken += k,
                // The server's plaintext buffer is full: read it first.
                Err(err) if err.kind() == io::ErrorKind::Other => {}
                Err(err) => panic!("server read_tls: {err}"),
            }
            if let Err(err) = self.server.process_new_packets() {
                self.server_error = Some(err);
            }
            self.server_read();
        }
        self.bufs.cipher_out.drain(..taken);
        self.moved += taken;
    }

    fn server_read(&mut self) {
        let mut buf = [0u8; 4096];
        loop {
            match self.server.reader().read(&mut buf) {
                Ok(0) => {
                    self.server_closed = true;
                    break;
                }
                Ok(n) => {
                    self.server_in.extend_from_slice(&buf[..n]);
                    self.moved += n;
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) => panic!("server reader: {err}"),
            }
        }
    }

    fn server_write(&mut self) {
        if !self.server_out.is_empty() && self.server_error.is_none() {
            let n = self
                .server
                .writer()
                .write(&self.server_out)
                .expect("server writer");
            self.server_out.drain(..n);
            self.moved += n;
        }
        while self.server.wants_write() {
            self.server
                .write_tls(&mut self.to_client)
                .expect("server write_tls");
        }
    }

    /// The network delivers part of what the server sent into `cipher_in`.
    fn server_to_client(&mut self) {
        let room = self.inbound_cap.saturating_sub(self.bufs.cipher_in.len());
        let n = self.cut(self.to_client.len().min(room));
        self.bufs.cipher_in.extend(self.to_client.drain(..n));
        self.moved += n;
    }

    fn client_read(&mut self) {
        if self.read_every == 0 || self.rounds % self.read_every != 0 {
            return;
        }
        self.moved += self.bufs.plain_in.len();
        self.client_in.append(&mut self.bufs.plain_in);
    }

    /// One trip around: returns whether anything moved.
    fn round(&mut self) -> bool {
        let before = (self.moved, self.lengths());
        self.rounds += 1;
        self.pump();
        self.client_to_server();
        self.server_write();
        self.server_to_client();
        self.pump();
        self.client_read();
        self.pump();
        (self.moved, self.lengths()) != before
    }

    fn lengths(&self) -> [usize; 5] {
        [
            self.bufs.cipher_in.len(),
            self.bufs.cipher_out.len(),
            self.bufs.plain_in.len(),
            self.bufs.plain_out.len(),
            self.to_client.len(),
        ]
    }

    /// Runs rounds until nothing moves. Some pauses (a reader that drains
    /// every few rounds) look like quiescence for a round or two, so it
    /// waits out a few idle rounds.
    fn run(&mut self) {
        let mut idle = 0;
        for _ in 0..2_000_000 {
            if self.round() {
                idle = 0;
            } else {
                idle += 1;
                if idle > self.read_every.max(1) + 1 {
                    return;
                }
            }
        }
        panic!("the network never went quiet");
    }

    fn status(&self) -> TlsStatus {
        match (&self.status, &self.error) {
            (_, Some(err)) => panic!("client failed: {err}"),
            (Some(status), None) => *status,
            (None, None) => panic!("never pumped"),
        }
    }

    fn error(&self) -> TlsError {
        self.error.clone().expect("the client should have failed")
    }

    /// Completes the handshake with no application data.
    fn handshake(&mut self) {
        self.run();
        assert!(!self.status().handshaking);
        assert!(self.server_error.is_none());
    }
}

fn tls13(pki: &Pki) -> Arc<ServerConfig> {
    server_config(pki, &[&TLS13])
}

fn tls12(pki: &Pki) -> Arc<ServerConfig> {
    server_config(pki, &[&TLS12])
}

fn trusted(pki: &Pki) -> Arc<ClientConfig> {
    client_config(Some(pki.roots.clone())).expect("client config")
}

// ---------------------------------------------------------------------------
// Handshake and data in both directions.
// ---------------------------------------------------------------------------

/// A request queued before the handshake and a response, in both
/// directions, then checks what was negotiated.
fn exchange(server: Arc<ServerConfig>, host: &'static str, cut: Cut, len: usize) -> Net {
    let mut net = Net::new(trusting(), host, server, cut);
    net.bufs.plain_out = payload(len, 1);
    net.server_out = payload(len, 2);
    net.run();
    let status = net.status();
    assert!(!status.handshaking);
    assert!(!status.peer_closed);
    assert!(status.wants_read);
    assert!(net.server_error.is_none());
    assert!(
        net.server_in == payload(len, 1),
        "request corrupted ({cut:?})"
    );
    assert!(
        net.client_in == payload(len, 2),
        "response corrupted ({cut:?})"
    );
    assert_eq!(net.server.alpn_protocol(), Some(&b"http/1.1"[..]));
    net
}

thread_local! {
    /// One PKI per test thread: key generation is the slow part of setup.
    static PKI: Pki = pki("steploop test CA");
}

fn with_pki<T>(f: impl FnOnce(&Pki) -> T) -> T {
    PKI.with(f)
}

/// A client config that trusts the test CA.
fn trusting() -> Arc<ClientConfig> {
    with_pki(trusted)
}

#[test]
fn tls13_exchange_whole() {
    let net = exchange(with_pki(tls13), "localhost", Cut::Whole, 50_000);
    assert_eq!(
        net.server.protocol_version(),
        Some(ProtocolVersion::TLSv1_3)
    );
}

#[test]
fn tls13_exchange_one_byte_at_a_time() {
    exchange(with_pki(tls13), "api.example.test", Cut::OneByte, 3_000);
}

#[test]
fn tls13_exchange_random_cuts() {
    for seed in 0..16 {
        let server = with_pki(tls13);
        let mut net = Net::new(
            trusting(),
            "api.example.test",
            server,
            Cut::Random { max: 1_500 },
        );
        net.rng = Rng(seed);
        net.bufs.plain_out = payload(40_000, seed);
        net.server_out = payload(40_000, !seed);
        net.run();
        assert!(!net.status().handshaking);
        assert!(
            net.server_in == payload(40_000, seed),
            "request corrupted (seed {seed})"
        );
        assert!(
            net.client_in == payload(40_000, !seed),
            "response corrupted (seed {seed})"
        );
    }
}

#[test]
fn tls12_exchange_whole() {
    let net = exchange(with_pki(tls12), "localhost", Cut::Whole, 50_000);
    assert_eq!(
        net.server.protocol_version(),
        Some(ProtocolVersion::TLSv1_2)
    );
}

#[test]
fn tls12_exchange_one_byte_at_a_time() {
    exchange(with_pki(tls12), "localhost", Cut::OneByte, 3_000);
}

#[test]
fn tls12_exchange_random_cuts() {
    exchange(
        with_pki(tls12),
        "api.example.test",
        Cut::Random { max: 700 },
        40_000,
    );
}

#[test]
fn the_first_pump_sends_the_client_hello() {
    let mut client = TlsClient::new(trusting(), name("localhost")).expect("client");
    let mut bufs = StageBufs::default();
    let status = client.pump(&mut bufs).expect("pump");
    assert!(status.handshaking);
    assert!(status.wants_write);
    assert!(status.wants_read);
    assert_eq!(bufs.cipher_out[0], 0x16, "a handshake record");
    // Nothing new to do: nothing moves.
    let before = bufs.clone();
    client.pump(&mut bufs).expect("pump");
    assert_eq!(bufs, before);
}

#[test]
fn plaintext_waits_in_plain_out_during_the_handshake() {
    let mut client = TlsClient::new(trusting(), name("localhost")).expect("client");
    let mut bufs = StageBufs {
        plain_out: b"GET / HTTP/1.1\r\n\r\n".to_vec(),
        ..StageBufs::default()
    };
    client.pump(&mut bufs).expect("pump");
    assert_eq!(bufs.plain_out, b"GET / HTTP/1.1\r\n\r\n");
}

// ---------------------------------------------------------------------------
// Buffer limits and backpressure.
// ---------------------------------------------------------------------------

/// A megabyte each way through small buffers, a slow reader, a small inbound
/// cap and random cuts: every pump stays within the limit, the stage holds
/// input back in both directions, and nothing is lost.
#[test]
fn large_payloads_respect_the_buffer_limit() {
    for (versions, seed) in [(&[&TLS13][..], 7), (&[&TLS12][..], 8)] {
        const LEN: usize = 1 << 20;
        let server = with_pki(|pki| server_config(pki, versions));
        let mut net = Net::with_limit(
            trusting(),
            "localhost",
            server,
            Cut::Random { max: 9_000 },
            4_096,
        );
        net.rng = Rng(seed);
        net.inbound_cap = 8_192;
        net.read_every = 3;
        net.bufs.plain_out = payload(LEN, seed);
        net.server_out = payload(LEN, !seed);
        net.run();
        assert!(net.server_in == payload(LEN, seed), "request corrupted");
        assert!(net.client_in == payload(LEN, !seed), "response corrupted");
        assert!(net.held_cipher_in, "never held ciphertext back");
        assert!(net.held_plain_out, "never held plaintext back");
    }
}

/// With the default limit, rustls's own 64 KiB buffers are the ones that
/// fill first.
#[test]
fn large_payloads_with_the_default_limit() {
    const LEN: usize = 1 << 20;
    let mut net = Net::new(trusting(), "localhost", with_pki(tls13), Cut::Whole);
    net.bufs.plain_out = payload(LEN, 3);
    net.server_out = payload(LEN, 4);
    net.run();
    assert!(net.server_in == payload(LEN, 3));
    assert!(net.client_in == payload(LEN, 4));
}

/// A one-byte limit still moves everything, a byte at a time.
#[test]
fn a_one_byte_limit_still_makes_progress() {
    let mut net = Net::with_limit(trusting(), "localhost", with_pki(tls13), Cut::Whole, 1);
    net.bufs.plain_out = payload(500, 5);
    net.server_out = payload(500, 6);
    net.run();
    assert!(net.server_in == payload(500, 5));
    assert!(net.client_in == payload(500, 6));
}

#[test]
fn a_stalled_reader_leaves_ciphertext_in_cipher_in() {
    let mut net = Net::with_limit(trusting(), "localhost", with_pki(tls13), Cut::Whole, 4_096);
    net.handshake();
    net.read_every = 0;
    net.server_out = payload(100_000, 9);
    net.run();
    let status = net.status();
    assert_eq!(net.bufs.plain_in.len(), 4_096);
    assert!(!net.bufs.cipher_in.is_empty());
    assert!(!status.wants_read);

    net.read_every = 1;
    net.run();
    assert!(net.client_in == payload(100_000, 9));
    assert!(net.bufs.cipher_in.is_empty());
    assert!(net.status().wants_read);
}

#[test]
fn a_stalled_socket_leaves_plaintext_in_plain_out() {
    let mut net = Net::with_limit(trusting(), "localhost", with_pki(tls13), Cut::Whole, 4_096);
    net.handshake();
    net.writing = false;
    net.bufs.plain_out = payload(100_000, 10);
    net.run();
    let status = net.status();
    assert!(status.wants_write);
    assert!(net.bufs.cipher_out.len() >= 4_096);
    assert!(net.bufs.plain_out.len() >= 100_000 - 4_096);

    net.writing = true;
    net.run();
    assert!(net.server_in == payload(100_000, 10));
    assert!(net.bufs.plain_out.is_empty());
}

// ---------------------------------------------------------------------------
// End of stream.
// ---------------------------------------------------------------------------

#[test]
fn close_notify_from_the_server_ends_the_stream() {
    for server in [with_pki(tls13), with_pki(tls12)] {
        let mut net = Net::new(trusting(), "localhost", server, Cut::Random { max: 300 });
        net.handshake();
        net.server_out = payload(10_000, 11);
        net.run();
        net.server.send_close_notify();
        net.run();
        let status = net.status();
        assert!(status.peer_closed);
        assert!(!status.wants_read);
        assert!(net.client_in == payload(10_000, 11));

        // Anything after close_notify is ignored, not an error.
        net.bufs
            .cipher_in
            .extend_from_slice(b"\x17\x03\x03\x00\x05junk!");
        net.pump();
        assert!(net.status().peer_closed);
        assert!(net.bufs.cipher_in.is_empty());
        assert!(net.bufs.plain_in.is_empty());
    }
}

#[test]
fn close_notify_waits_for_the_reader() {
    let mut net = Net::with_limit(trusting(), "localhost", with_pki(tls13), Cut::Whole, 1_000);
    net.handshake();
    net.read_every = 0;
    net.server_out = payload(5_000, 12);
    net.server_write();
    net.server.send_close_notify();
    net.run();
    assert!(
        !net.status().peer_closed,
        "closed with bytes still in rustls"
    );

    net.read_every = 1;
    net.run();
    assert!(net.status().peer_closed);
    assert!(net.client_in == payload(5_000, 12));
}

#[test]
fn a_bare_fin_after_the_handshake_ends_the_stream() {
    for server in [with_pki(tls13), with_pki(tls12)] {
        let mut net = Net::new(trusting(), "localhost", server, Cut::Random { max: 300 });
        net.handshake();
        net.server_out = payload(10_000, 13);
        net.run();
        assert!(net.status().wants_read);

        net.client.peer_eof();
        net.pump();
        let status = net.status();
        assert!(status.peer_closed);
        assert!(!status.wants_read);
        assert!(net.client_in == payload(10_000, 13));
    }
}

#[test]
fn a_fin_waits_for_held_back_ciphertext() {
    let mut net = Net::with_limit(trusting(), "localhost", with_pki(tls13), Cut::Whole, 1_000);
    net.handshake();
    net.read_every = 0;
    net.server_out = payload(20_000, 14);
    net.run();
    assert!(!net.bufs.cipher_in.is_empty());

    net.client.peer_eof();
    net.pump();
    assert!(
        !net.status().peer_closed,
        "closed with bytes still in cipher_in"
    );

    net.read_every = 1;
    net.run();
    assert!(net.status().peer_closed);
    assert!(net.client_in == payload(20_000, 14));
}

#[test]
fn a_fin_partway_through_a_record_is_an_error() {
    let mut net = Net::new(trusting(), "localhost", with_pki(tls13), Cut::Whole);
    net.handshake();
    net.server_out = payload(1_000, 15);
    net.server_write();
    let len = net.to_client.len();
    net.bufs.cipher_in.extend(net.to_client.drain(..len - 3));
    net.client.peer_eof();
    net.pump();
    assert!(
        net.error()
            .message()
            .contains("partway through a TLS record")
    );
}

#[test]
fn a_fin_during_the_handshake_is_an_error() {
    // Before the server says anything.
    let mut client = TlsClient::new(trusting(), name("localhost")).expect("client");
    let mut bufs = StageBufs::default();
    client.pump(&mut bufs).expect("pump");
    client.peer_eof();
    let err = client
        .pump(&mut bufs)
        .expect_err("FIN during the handshake");
    assert!(err.message().contains("during the TLS handshake"), "{err}");
    // Errors are final.
    assert_eq!(client.pump(&mut bufs), Err(err));

    // Partway through the server's flight.
    let mut net = Net::new(trusting(), "localhost", with_pki(tls13), Cut::Whole);
    net.pump();
    net.client_to_server();
    net.server_write();
    let half = net.to_client.len() / 2;
    net.bufs.cipher_in.extend(net.to_client.drain(..half));
    net.pump();
    assert!(net.status().handshaking);
    net.client.peer_eof();
    net.pump();
    assert!(net.error().message().contains("during the TLS handshake"));
}

// ---------------------------------------------------------------------------
// Closing from the client.
// ---------------------------------------------------------------------------

#[test]
fn close_sends_close_notify_after_pending_plaintext() {
    let mut net = Net::new(
        trusting(),
        "localhost",
        with_pki(tls13),
        Cut::Random { max: 200 },
    );
    net.bufs.plain_out = payload(20_000, 16);
    net.client.close();
    net.run();
    assert!(net.server_in == payload(20_000, 16));
    assert!(net.server_closed);
    let status = net.status();
    assert!(status.close_sent);
    assert!(!status.wants_write);

    // Nothing more is taken from plain_out.
    net.bufs.plain_out.extend_from_slice(b"late");
    net.run();
    assert_eq!(net.bufs.plain_out, b"late");
}

// ---------------------------------------------------------------------------
// Handshake failures.
// ---------------------------------------------------------------------------

#[test]
fn a_wrong_server_name_fails_the_handshake() {
    let mut net = Net::new(
        trusting(),
        "other.example.test",
        with_pki(tls13),
        Cut::Whole,
    );
    net.run();
    let err = net.error();
    assert!(err.message().contains("not valid for name"), "{err}");
    // The alert queued on failure reaches the server.
    assert!(
        matches!(net.server_error, Some(rustls::Error::AlertReceived(_))),
        "{:?}",
        net.server_error
    );
}

#[test]
fn an_unknown_ca_fails_the_handshake() {
    let stranger = pki("a stranger CA");
    let mut net = Net::new(trusted(&stranger), "localhost", with_pki(tls13), Cut::Whole);
    net.run();
    let err = net.error();
    assert!(err.message().contains("UnknownIssuer"), "{err}");
    assert!(matches!(
        net.server_error,
        Some(rustls::Error::AlertReceived(_))
    ));
}

// ---------------------------------------------------------------------------
// The config.
// ---------------------------------------------------------------------------

#[test]
fn the_webpki_config_builds() {
    let config = client_config(None).expect("webpki config");
    assert_eq!(config.alpn_protocols, vec![b"http/1.1".to_vec()]);
    let mut client = TlsClient::new(config, name("api.anthropic.com")).expect("client");
    let mut bufs = StageBufs::default();
    let status = client.pump(&mut bufs).expect("pump");
    assert!(status.handshaking);
    assert_eq!(bufs.cipher_out[0], 0x16);
}

/// rustls keeps its resumption cache in the shared config; the stage turns
/// it off, so a second connection does a full handshake like the first.
#[test]
fn connections_sharing_a_config_do_not_resume() {
    let client = trusting();
    let server = with_pki(tls13);
    for _ in 0..2 {
        let mut net = Net::new(client.clone(), "localhost", server.clone(), Cut::Whole);
        net.server_out = b"ticket time".to_vec();
        net.run();
        assert_eq!(net.client_in, b"ticket time");
        assert_eq!(net.server.handshake_kind(), Some(HandshakeKind::Full));
    }
}
