//! The TLS client stage: a pure byte stage over rustls's buffered
//! [`ClientConnection`]. See `docs/explanation/sans-io-shell.md` §4.5 and
//! §5.5.
//!
//! ```text
//! socket ─▶ cipher_in  ─▶ [TlsClient] ─▶ plain_in  ─▶ HTTP parser
//! socket ◀─ cipher_out ◀─ [TlsClient] ◀─ plain_out ◀─ HTTP writer
//! ```
//!
//! The stage knows nothing of sockets, ids or the reactor, so the same code
//! runs under the loop and in tests that play the network by hand.
//! [`TlsClient::pump`] moves bytes between the four [`StageBufs`] and reports
//! a [`TlsStatus`]; the TCP stage fills `cipher_in` and drains `cipher_out`,
//! and the HTTP stage drains `plain_in` and fills `plain_out`.
//!
//! # Purity
//!
//! The stage does no I/O and keeps no state outside itself, with the two
//! exceptions §4.5 documents, both inside rustls:
//!
//! - **Entropy.** rustls draws the client random and key shares from the OS.
//! - **Wall clock.** rustls reads the time to check certificate validity.
//!
//! So its control flow is deterministic but its bytes are not. Session
//! resumption is off: rustls keeps its session cache behind a mutex in the
//! shared `ClientConfig`, which would let one connection's handshake depend
//! on another's. (rustls also calls the `log` facade, which does nothing
//! unless the embedder installs a logger.)
//!
//! # Backpressure
//!
//! Both directions leave what the next stage cannot take where it is, so the
//! stages around this one see it:
//!
//! - rustls gets more ciphertext only once it has handed over everything it
//!   decrypted, and `plain_in` grows only up to the buffer limit. A slow
//!   reader leaves ciphertext in `cipher_in`, and the TCP stage stops reading.
//! - Plaintext is encrypted only while `cipher_out` is under the limit, at
//!   most as much as fits, so `cipher_out` overshoots it by record overhead
//!   alone (handshake messages and alerts are never held back). A slow socket
//!   leaves plaintext in `plain_out`, where the HTTP writer sees it.
//!
//! # End of stream
//!
//! The peer's stream ends normally ([`TlsStatus::peer_closed`]) on either:
//!
//! - **close_notify.** Anything after it is discarded, as RFC 8446 §6.1
//!   requires.
//! - **A bare TCP FIN after the handshake,** at a record boundary. rustls
//!   calls that an unexpected EOF, since it lets an attacker truncate the
//!   stream, but many servers close without close_notify, and HTTP clients
//!   sending `Connection: close` (jig's C port among them) accept it. A body
//!   with a declared length is still checked by the HTTP framing above.
//!
//! A FIN during the handshake, or partway through a record, is an error.

use std::fmt;
use std::io::{self, BufRead, Write};
use std::sync::Arc;

use rustls::client::Resumption;
use rustls::{ClientConfig, ClientConnection, RootCertStore};
use rustls_pki_types::ServerName;

/// How far `pump` lets `plain_in` and `cipher_out` grow by default: rustls's
/// own default buffer limit.
pub const DEFAULT_BUFFER_LIMIT: usize = 64 * 1024;

/// The client config the stage is meant for: the `ring` provider, rustls's
/// safe default protocol versions, and `roots` as the trust anchors, or the
/// webpki roots (Mozilla's set, compiled in) when `None`.
///
/// ALPN offers only `http/1.1`. The client speaks HTTP/1.1 alone, and
/// offering `h2` lets servers negotiate HTTP/2 and then abort on the HTTP/1.1
/// request text. Resumption is disabled (see the module doc).
///
/// Build it once and share the `Arc` between connections. It fails only if
/// the provider cannot support the default protocol versions, which `ring`
/// always does.
pub fn client_config(roots: Option<RootCertStore>) -> Result<Arc<ClientConfig>, TlsError> {
    let roots = roots.unwrap_or_else(|| RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    });
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    config.resumption = Resumption::disabled();
    Ok(Arc::new(config))
}

/// The buffers a [`TlsClient`] moves bytes between. `pump` consumes from the
/// front of `cipher_in` and `plain_out` and appends to `plain_in` and
/// `cipher_out`; the stages on either side do the opposite.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StageBufs {
    /// Ciphertext from the socket.
    pub cipher_in: Vec<u8>,
    /// Ciphertext for the socket.
    pub cipher_out: Vec<u8>,
    /// Decrypted bytes for the layer above.
    pub plain_in: Vec<u8>,
    /// Bytes from the layer above, to be encrypted.
    pub plain_out: Vec<u8>,
}

/// Where the stage stands after a [`TlsClient::pump`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TlsStatus {
    /// The handshake is not finished: `plain_out` waits, and nothing arrives
    /// in `plain_in`.
    pub handshaking: bool,
    /// The peer's stream has ended normally, by close_notify or by a FIN
    /// accepted as closure, and everything it sent is in `plain_in`.
    pub peer_closed: bool,
    /// The stage would take more ciphertext: the TCP stage should keep
    /// reading. False once the stream has ended, after a FIN, and while
    /// `plain_in` is at the buffer limit.
    pub wants_read: bool,
    /// `cipher_out` holds bytes for the socket.
    pub wants_write: bool,
    /// close_notify is in `cipher_out` (or has left it), after everything
    /// `plain_out` held. Once `cipher_out` is written, the socket can close.
    pub close_sent: bool,
}

/// Why the stage failed: a handshake failure (an unknown CA, a name
/// mismatch, a fatal alert from the server), a protocol error, or a FIN
/// during the handshake or partway through a record. The message is
/// rustls's own where rustls found the problem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TlsError {
    message: String,
}

impl TlsError {
    fn new(message: impl Into<String>) -> TlsError {
        TlsError {
            message: message.into(),
        }
    }

    /// What went wrong, as `Display` shows it.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for TlsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TlsError {}

impl From<rustls::Error> for TlsError {
    fn from(err: rustls::Error) -> TlsError {
        TlsError::new(err.to_string())
    }
}

impl From<io::Error> for TlsError {
    fn from(err: io::Error) -> TlsError {
        TlsError::new(err.to_string())
    }
}

/// One TLS client connection as a byte stage.
///
/// Call [`pump`](TlsClient::pump) once after creating it, to put the
/// ClientHello in `cipher_out`, and again whenever any of the buffers or
/// [`peer_eof`](TlsClient::peer_eof) and [`close`](TlsClient::close) change
/// something. A pump with nothing to do is cheap.
///
/// Errors are final: after the first, every `pump` returns the same error and
/// moves nothing.
#[derive(Debug)]
pub struct TlsClient {
    /// Boxed: it is over a kilobyte, and the stage sits in an enum beside a
    /// plain link (§5.5).
    conn: Box<ClientConnection>,
    limit: usize,
    records: Records,
    /// The TCP peer sent FIN. Acted on once every byte before it is used.
    eof: bool,
    /// A close_notify arrived.
    close_notify: bool,
    /// The peer's stream has ended and every byte of it is in `plain_in`.
    ended: bool,
    /// [`TlsClient::close`] was called.
    close_requested: bool,
    close_sent: bool,
    failed: Option<TlsError>,
}

impl TlsClient {
    /// A connection to `server_name`, which is checked against the server's
    /// certificate and sent as SNI unless it is an IP address.
    pub fn new(
        config: Arc<ClientConfig>,
        server_name: ServerName<'static>,
    ) -> Result<TlsClient, TlsError> {
        Ok(TlsClient {
            conn: Box::new(ClientConnection::new(config, server_name)?),
            limit: DEFAULT_BUFFER_LIMIT,
            records: Records::default(),
            eof: false,
            close_notify: false,
            ended: false,
            close_requested: false,
            close_sent: false,
            failed: None,
        })
    }

    /// Caps how far `pump` lets `plain_in` and `cipher_out` grow, instead of
    /// [`DEFAULT_BUFFER_LIMIT`]. A limit of zero counts as one byte, so that
    /// data still moves.
    pub fn with_buffer_limit(mut self, limit: usize) -> TlsClient {
        self.limit = limit.max(1);
        self
    }

    /// Tells the stage that the TCP peer sent FIN. The next `pump` that has
    /// used every byte received before it ends the stream if the handshake is
    /// done and the last record is complete, and fails otherwise. See the
    /// module doc for why a FIN without close_notify is accepted.
    pub fn peer_eof(&mut self) {
        self.eof = true;
    }

    /// Asks for close_notify. It goes into `cipher_out` behind everything
    /// already in `plain_out`, once the handshake is done, and from then on
    /// the stage takes nothing more from `plain_out`. To abandon a connection
    /// mid-handshake, close the socket instead.
    pub fn close(&mut self) {
        self.close_requested = true;
    }

    /// Moves as many bytes as it can through the connection: ciphertext from
    /// `cipher_in` through rustls to `plain_in`, then plaintext from
    /// `plain_out` to `cipher_out`, along with whatever rustls itself has to
    /// send. What cannot move yet stays at the front of its buffer.
    ///
    /// On failure `cipher_out` may end with a fatal alert telling the server
    /// why; sending it before closing is polite but optional.
    pub fn pump(&mut self, bufs: &mut StageBufs) -> Result<TlsStatus, TlsError> {
        if let Some(err) = &self.failed {
            return Err(err.clone());
        }
        let moved = self
            .read_side(&mut bufs.cipher_in, &mut bufs.plain_in)
            .and_then(|()| self.write_side(&mut bufs.plain_out, &mut bufs.cipher_out));
        // Flush even after a failure: rustls queues the alert that tells the
        // peer what went wrong.
        let flushed = flush(&mut self.conn, &mut bufs.cipher_out);
        match moved.and(flushed) {
            Ok(()) => Ok(self.status(bufs)),
            Err(err) => {
                self.failed = Some(err.clone());
                Err(err)
            }
        }
    }

    fn status(&self, bufs: &StageBufs) -> TlsStatus {
        TlsStatus {
            handshaking: self.conn.is_handshaking(),
            peer_closed: self.ended,
            wants_read: !self.ended && !self.eof && bufs.plain_in.len() < self.limit,
            wants_write: !bufs.cipher_out.is_empty(),
            close_sent: self.close_sent,
        }
    }

    fn read_side(
        &mut self,
        cipher_in: &mut Vec<u8>,
        plain_in: &mut Vec<u8>,
    ) -> Result<(), TlsError> {
        let mut used = 0;
        let result = loop {
            if !deliver(&mut self.conn, plain_in, self.limit) {
                // rustls holds plaintext back; feeding it more would only
                // grow its buffers.
                break Ok(());
            }
            if self.close_notify {
                used = cipher_in.len();
                self.ended = true;
                break Ok(());
            }
            let rest = &cipher_in[used..];
            if rest.is_empty() {
                break self.end_on_eof();
            }
            let mut reader = rest;
            let n = match self.conn.read_tls(&mut reader) {
                Ok(0) => break Ok(()),
                Ok(n) => n,
                Err(err) => break Err(err.into()),
            };
            self.records.advance(&rest[..n]);
            used += n;
            match self.conn.process_new_packets() {
                Ok(state) => self.close_notify = state.peer_has_closed(),
                Err(err) => break Err(err.into()),
            }
        };
        cipher_in.drain(..used);
        result
    }

    /// Acts on a FIN once rustls has used and delivered everything before it.
    fn end_on_eof(&mut self) -> Result<(), TlsError> {
        if !self.eof || self.ended {
            return Ok(());
        }
        if self.conn.is_handshaking() {
            return Err(TlsError::new(
                "the peer closed the connection during the TLS handshake",
            ));
        }
        if !self.records.at_boundary() {
            return Err(TlsError::new(
                "the peer closed the connection partway through a TLS record",
            ));
        }
        self.ended = true;
        Ok(())
    }

    fn write_side(
        &mut self,
        plain_out: &mut Vec<u8>,
        cipher_out: &mut Vec<u8>,
    ) -> Result<(), TlsError> {
        // rustls would buffer plaintext written during the handshake. Leaving
        // it in `plain_out` keeps the one queue the HTTP writer can see.
        if self.conn.is_handshaking() || self.close_sent {
            return Ok(());
        }
        let (used, result) = encrypt(&mut self.conn, plain_out, cipher_out, self.limit);
        plain_out.drain(..used);
        result?;
        if self.close_requested && plain_out.is_empty() {
            self.conn.send_close_notify();
            self.close_sent = true;
        }
        Ok(())
    }
}

/// Moves decrypted bytes from rustls to `plain_in` while `plain_in` is under
/// `limit`. Returns whether rustls has none left.
fn deliver(conn: &mut ClientConnection, plain_in: &mut Vec<u8>, limit: usize) -> bool {
    let mut reader = conn.reader();
    loop {
        // An error is `WouldBlock`, for nothing buffered. (`UnexpectedEof`
        // cannot happen: the stage never passes a FIN to rustls.)
        let chunk = match reader.fill_buf() {
            Ok(chunk) if !chunk.is_empty() => chunk,
            _ => return true,
        };
        let room = limit.saturating_sub(plain_in.len());
        if room == 0 {
            return false;
        }
        let n = room.min(chunk.len());
        plain_in.extend_from_slice(&chunk[..n]);
        reader.consume(n);
    }
}

/// Encrypts from the front of `plain` into `cipher_out` while `cipher_out` is
/// under `limit`, never more than fits. Returns how much of `plain` was used,
/// which the caller must drop even on error.
fn encrypt(
    conn: &mut ClientConnection,
    plain: &[u8],
    cipher_out: &mut Vec<u8>,
    limit: usize,
) -> (usize, Result<(), TlsError>) {
    let mut used = 0;
    while used < plain.len() && cipher_out.len() < limit {
        let end = plain.len().min(used + (limit - cipher_out.len()));
        match conn.writer().write(&plain[used..end]) {
            // rustls's own outgoing buffer is full; `flush` empties it every
            // time, so this is defensive.
            Ok(0) => break,
            Ok(n) => used += n,
            Err(err) => return (used, Err(err.into())),
        }
        if let Err(err) = flush(conn, cipher_out) {
            return (used, Err(err));
        }
    }
    (used, Ok(()))
}

/// Moves every record rustls has queued into `cipher_out`.
fn flush(conn: &mut ClientConnection, cipher_out: &mut Vec<u8>) -> Result<(), TlsError> {
    while conn.wants_write() {
        if conn.write_tls(cipher_out)? == 0 {
            break;
        }
    }
    Ok(())
}

/// Tracks where the ciphertext fed to rustls stands against record
/// boundaries, so a FIN between records can be told from a truncated record:
/// rustls keeps a partial record to itself and says nothing about it.
#[derive(Clone, Copy, Debug, Default)]
struct Records {
    /// Bytes of the current record's 5-byte header seen so far.
    header_seen: usize,
    /// The header's length field, once its high byte has arrived.
    length: usize,
    /// Body bytes of the current record still to come.
    body_left: usize,
}

impl Records {
    fn advance(&mut self, mut bytes: &[u8]) {
        while let Some((&byte, rest)) = bytes.split_first() {
            if self.body_left > 0 {
                let n = self.body_left.min(bytes.len());
                self.body_left -= n;
                bytes = &bytes[n..];
                continue;
            }
            // A header is a content type, two version bytes and a big-endian
            // body length.
            match self.header_seen {
                3 => self.length = usize::from(byte) << 8,
                4 => self.body_left = self.length | usize::from(byte),
                _ => {}
            }
            self.header_seen = (self.header_seen + 1) % 5;
            bytes = rest;
        }
    }

    fn at_boundary(&self) -> bool {
        self.header_seen == 0 && self.body_left == 0
    }
}
