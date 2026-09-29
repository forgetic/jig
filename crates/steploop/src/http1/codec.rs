//! The HTTP/1.1 codec: pure functions from bytes to heads and back, plus an
//! incremental chunked decoder.
//!
//! Heads go through `httparse`, which is incremental, allocation-free and
//! keeps header order and case. Framing is our own small code, so the
//! planners decide every byte they put on the wire: the core supplies the
//! exact status line and header list, and [`write_response`] only serializes
//! it. That is what keeps jig's responses byte-identical to the async server
//! it replaces. See `docs/explanation/sans-io-shell.md` §4.6.
//!
//! # Strictness
//!
//! The old async readers (at `ca1edfd`) were lenient: they split on `\r\n`,
//! skipped lines without a colon, took the last `Content-Length` and read an
//! unparseable one as zero. That misreads hostile or broken input silently.
//! These parsers accept less, and for every head they accept they produce
//! the same method, target, headers and body length as the old readers did:
//!
//! - Every line must end in CRLF. `httparse` also accepts a bare LF, which
//!   the old readers never recognised as a line end, so it is rejected here
//!   to keep one notion of where a head ends.
//! - The request target must be ASCII, as RFC 3986 requires. The old readers
//!   split the request line on Unicode whitespace, so a non-ASCII target
//!   could be cut short.
//! - `Content-Length` must be digits only, and repeated fields must agree.
//! - A request `Transfer-Encoding` is refused with `501` rather than read as
//!   an empty body.
//!
//! One deliberate difference remains: empty lines before a request line are
//! skipped, as RFC 9112 §2.2 recommends and `httparse` does. The old readers
//! took such a line as an empty request line.
//!
//! Header values are decoded as lossy UTF-8 and trimmed with [`str::trim`],
//! exactly as the old readers did, so obs-text and Unicode whitespace come out
//! the same.
//!
//! # Cost of reparsing
//!
//! The head parsers are stateless: the planner calls them again with the
//! whole buffer after each read, so each call costs O(buffer). A head can
//! only complete at a newline, so a planner that reparses only when the new
//! bytes contain `b'\n'`, or when the buffer reaches `Limits::max_head`,
//! bounds the work to one parse per head line.

use std::fmt;

use super::{Body, Request, Response};

/// Bounds on what a peer can make a head parser accept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Bytes in a head, from the first byte up to and including the blank
    /// line that ends it.
    pub max_head: usize,
    /// Bytes a request may declare in `Content-Length`. The server planner
    /// buffers the whole body, so this bounds its memory per connection.
    /// Response bodies stream, so the response parser ignores it.
    pub max_body: usize,
    /// Header fields in one head. The parser allocates this many slots per
    /// call, so keep it small.
    pub max_headers: usize,
}

impl Default for Limits {
    /// 64 KiB heads, 64 MiB request bodies, 100 header fields: generous for
    /// LLM traffic, small enough that one connection can't exhaust memory.
    fn default() -> Self {
        Limits {
            max_head: 64 * 1024,
            max_body: 64 * 1024 * 1024,
            max_headers: 100,
        }
    }
}

/// Why a head was refused. [`HeadError::status`] gives the response a server
/// sends back; a client reports it as a failed fetch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeadError {
    /// Not HTTP/1.x syntax. The text says what was wrong.
    Malformed(&'static str),
    /// The head is longer than `Limits::max_head`.
    HeadTooLarge,
    /// The head has more than `Limits::max_headers` fields.
    TooManyHeaders,
    /// A request declares a body longer than `Limits::max_body`.
    BodyTooLarge,
    /// A request uses `Transfer-Encoding`, which the server does not decode.
    TransferEncoding,
    /// A `Content-Length` that is not a plain decimal number, or several that
    /// disagree. The message's framing is unknowable (RFC 9112 §6.3).
    BadContentLength,
}

impl HeadError {
    /// The status code and reason phrase a server answers this error with.
    pub fn status(&self) -> (u16, &'static str) {
        match self {
            HeadError::Malformed(_) | HeadError::BadContentLength => (400, "Bad Request"),
            HeadError::HeadTooLarge | HeadError::TooManyHeaders | HeadError::BodyTooLarge => {
                (413, "Content Too Large")
            }
            HeadError::TransferEncoding => (501, "Not Implemented"),
        }
    }
}

impl fmt::Display for HeadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HeadError::Malformed(why) => write!(f, "malformed HTTP head: {why}"),
            HeadError::HeadTooLarge => f.write_str("HTTP head too large"),
            HeadError::TooManyHeaders => f.write_str("too many header fields"),
            HeadError::BodyTooLarge => f.write_str("declared body too large"),
            HeadError::TransferEncoding => f.write_str("request Transfer-Encoding not supported"),
            HeadError::BadContentLength => f.write_str("invalid or conflicting Content-Length"),
        }
    }
}

impl std::error::Error for HeadError {}

/// How long a request body is, from its head. Requests without
/// `Content-Length` have no body (a `Transfer-Encoding` is refused).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyLength {
    /// No `Content-Length` field.
    None,
    /// `Content-Length: n`, already checked against `Limits::max_body`.
    Length(usize),
}

/// A parsed request head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestHead {
    pub method: String,
    /// The request target as sent, query string included.
    pub target: String,
    /// Names as sent; values lossy UTF-8, trimmed of surrounding whitespace.
    pub headers: Vec<(String, String)>,
    /// Bytes from the start of the buffer up to and including the CRLF CRLF
    /// that ends the head. The body starts here.
    pub head_len: usize,
    pub body: BodyLength,
}

impl RequestHead {
    /// The number of body bytes that follow the head.
    pub fn body_len(&self) -> usize {
        match self.body {
            BodyLength::None => 0,
            BodyLength::Length(n) => n,
        }
    }

    /// Pair the head with its body.
    pub fn into_request(self, body: Vec<u8>) -> Request {
        Request {
            method: self.method,
            target: self.target,
            headers: self.headers,
            body,
        }
    }
}

/// How a response body is delimited, from its head (RFC 9112 §6.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Framing {
    /// No body: a `1xx`, `204` or `304` status. A response to `HEAD` has no
    /// body either, but only the caller knows the request method.
    Empty,
    /// Exactly this many bytes.
    Length(u64),
    /// `Transfer-Encoding` ending in `chunked`: use a [`ChunkedDecoder`].
    Chunked,
    /// Everything until the peer closes: no framing fields, or a
    /// `Transfer-Encoding` whose last coding is not `chunked`.
    UntilEof,
}

/// A parsed response head. `buf[..head_len]` is the raw head, for relaying
/// verbatim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResponseHead {
    pub status: u16,
    /// The reason phrase, or empty if it contains non-ASCII bytes.
    pub reason: String,
    /// Names as sent; values lossy UTF-8, trimmed of surrounding whitespace.
    pub headers: Vec<(String, String)>,
    /// Bytes from the start of the buffer up to and including the CRLF CRLF
    /// that ends the head.
    pub head_len: usize,
    pub framing: Framing,
}

/// Parse a request head from the start of `buf`.
///
/// `Ok(None)` means the head is not complete yet: read more and call again
/// with the whole buffer. Bytes after the head (the body) are ignored.
pub fn parse_request_head(buf: &[u8], limits: &Limits) -> Result<Option<RequestHead>, HeadError> {
    let mut slots = vec![httparse::EMPTY_HEADER; limits.max_headers];
    let mut req = httparse::Request::new(&mut slots);
    let Some(head_len) = run_parser(buf, limits, |window| req.parse(window))? else {
        return Ok(None);
    };
    require_crlf(&buf[..head_len])?;

    // Both fields are set once `parse` reports a complete head.
    let (Some(method), Some(target)) = (req.method, req.path) else {
        return Err(HeadError::Malformed("incomplete request line"));
    };
    if !target.is_ascii() {
        return Err(HeadError::Malformed("non-ASCII request target"));
    }
    let headers = owned_headers(req.headers);

    if headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("transfer-encoding"))
    {
        return Err(HeadError::TransferEncoding);
    }
    let body = match content_length(&headers)? {
        ContentLength::Absent => BodyLength::None,
        ContentLength::TooLarge => return Err(HeadError::BodyTooLarge),
        ContentLength::Value(n) => match usize::try_from(n) {
            Ok(n) if n <= limits.max_body => BodyLength::Length(n),
            _ => return Err(HeadError::BodyTooLarge),
        },
    };

    Ok(Some(RequestHead {
        method: method.to_owned(),
        target: target.to_owned(),
        headers,
        head_len,
        body,
    }))
}

/// Parse a response head from the start of `buf`.
///
/// `Ok(None)` means the head is not complete yet. `Limits::max_head` and
/// `Limits::max_headers` apply; `Limits::max_body` does not, since response
/// bodies stream.
pub fn parse_response_head(buf: &[u8], limits: &Limits) -> Result<Option<ResponseHead>, HeadError> {
    let mut slots = vec![httparse::EMPTY_HEADER; limits.max_headers];
    let mut res = httparse::Response::new(&mut slots);
    let Some(head_len) = run_parser(buf, limits, |window| res.parse(window))? else {
        return Ok(None);
    };
    require_crlf(&buf[..head_len])?;

    let (Some(status), Some(reason)) = (res.code, res.reason) else {
        return Err(HeadError::Malformed("incomplete status line"));
    };
    let headers = owned_headers(res.headers);
    let framing = response_framing(status, &headers)?;

    Ok(Some(ResponseHead {
        status,
        reason: reason.to_owned(),
        headers,
        head_len,
        framing,
    }))
}

/// Serialize a response exactly as given: the status line, each header in
/// order, a blank line, then the body.
///
/// The codec adds no header of its own: the core is responsible for framing
/// headers that match `body` (`Content-Length` for `Full`,
/// `Transfer-Encoding: chunked` for `Chunked`).
///
/// Each `Chunked` part is framed as one chunk, `{len:x}\r\n{part}\r\n`, and
/// the body ends with `0\r\n\r\n`. An empty part is framed literally too, as
/// `0\r\n\r\n`, which a client reads as the end of the body, so everything
/// after it is lost. This reproduces the old server's bytes for an empty SSE
/// body; cores should otherwise leave empty parts out.
pub fn write_response(response: &Response) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {} {}\r\n", response.status, response.reason).into_bytes();
    out.reserve(headers_size(&response.headers) + body_size(&response.body));
    put_headers(&mut out, &response.headers);
    match &response.body {
        Body::Empty => {}
        Body::Full(bytes) => out.extend_from_slice(bytes),
        Body::Chunked(parts) => {
            for part in parts {
                out.extend_from_slice(format!("{:x}\r\n", part.len()).as_bytes());
                out.extend_from_slice(part);
                out.extend_from_slice(b"\r\n");
            }
            out.extend_from_slice(b"0\r\n\r\n");
        }
    }
    out
}

/// Serialize a request head exactly as given: `{method} {target} HTTP/1.1`,
/// each header in order, then the blank line. The body, if any, follows
/// separately, framed by the headers the caller chose.
pub fn write_request_head(method: &str, target: &str, headers: &[(String, String)]) -> Vec<u8> {
    let mut out = format!("{method} {target} HTTP/1.1\r\n").into_bytes();
    out.reserve(headers_size(headers));
    put_headers(&mut out, headers);
    out
}

/// Runs an `httparse` parse over at most `max_head` bytes and maps its
/// outcome: `Some(head_len)` when complete, `None` when more bytes may help.
fn run_parser<'b>(
    buf: &'b [u8],
    limits: &Limits,
    parse: impl FnOnce(&'b [u8]) -> httparse::Result<usize>,
) -> Result<Option<usize>, HeadError> {
    let window = &buf[..buf.len().min(limits.max_head)];
    match parse(window) {
        Ok(httparse::Status::Complete(head_len)) => Ok(Some(head_len)),
        // Whatever completes this head would take it past the limit, so say
        // so now: the planner may have stopped reading at the limit.
        Ok(httparse::Status::Partial) if buf.len() >= limits.max_head => {
            Err(HeadError::HeadTooLarge)
        }
        Ok(httparse::Status::Partial) => Ok(None),
        Err(httparse::Error::TooManyHeaders) => Err(HeadError::TooManyHeaders),
        Err(err) => Err(HeadError::Malformed(match err {
            httparse::Error::HeaderName => "invalid header name",
            httparse::Error::HeaderValue => "invalid header value",
            httparse::Error::NewLine => "invalid line ending",
            httparse::Error::Status => "invalid status line",
            httparse::Error::Token => "invalid method or request target",
            httparse::Error::Version => "invalid HTTP version",
            httparse::Error::TooManyHeaders => "too many header fields",
        })),
    }
}

/// Rejects a head with a bare LF, which `httparse` accepts as a line end.
/// (It already rejects a bare CR.)
fn require_crlf(head: &[u8]) -> Result<(), HeadError> {
    let bare_lf = head
        .iter()
        .enumerate()
        .any(|(i, &b)| b == b'\n' && (i == 0 || head[i - 1] != b'\r'));
    if bare_lf {
        Err(HeadError::Malformed("line not ended by CRLF"))
    } else {
        Ok(())
    }
}

/// Copies parsed fields out, decoding and trimming values the way the old
/// readers did (lossy UTF-8, then Unicode `trim`), so obs-text and non-ASCII
/// whitespace come out identical.
fn owned_headers(headers: &[httparse::Header<'_>]) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|h| {
            let value = String::from_utf8_lossy(h.value);
            (h.name.to_owned(), value.trim().to_owned())
        })
        .collect()
}

/// The agreed `Content-Length` of a head.
enum ContentLength {
    Absent,
    Value(u64),
    /// Digits only, but more than `u64` holds.
    TooLarge,
}

/// Reads every `Content-Length` field. Each must be `1*DIGIT` (no sign, no
/// list), and repeated fields must hold the same number.
fn content_length(headers: &[(String, String)]) -> Result<ContentLength, HeadError> {
    let mut agreed = ContentLength::Absent;
    for (_, value) in headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
    {
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
            return Err(HeadError::BadContentLength);
        }
        let this = value
            .parse::<u64>()
            .map_or(ContentLength::TooLarge, ContentLength::Value);
        agreed = match (agreed, this) {
            (ContentLength::Absent, this) => this,
            (ContentLength::Value(a), ContentLength::Value(b)) if a == b => ContentLength::Value(a),
            (ContentLength::TooLarge, ContentLength::TooLarge) => ContentLength::TooLarge,
            _ => return Err(HeadError::BadContentLength),
        };
    }
    Ok(agreed)
}

/// RFC 9112 §6.3, minus the request-method rule the codec cannot see.
fn response_framing(status: u16, headers: &[(String, String)]) -> Result<Framing, HeadError> {
    if (100..200).contains(&status) || status == 204 || status == 304 {
        return Ok(Framing::Empty);
    }
    let mut codings = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("transfer-encoding"))
        .flat_map(|(_, value)| value.split(','))
        .map(str::trim)
        .filter(|coding| !coding.is_empty())
        .peekable();
    if codings.peek().is_some() {
        // Transfer-Encoding overrides Content-Length (§6.3 rule 3).
        let chunked = codings
            .last()
            .is_some_and(|coding| coding.eq_ignore_ascii_case("chunked"));
        return Ok(if chunked {
            Framing::Chunked
        } else {
            Framing::UntilEof
        });
    }
    match content_length(headers)? {
        ContentLength::Absent => Ok(Framing::UntilEof),
        ContentLength::Value(n) => Ok(Framing::Length(n)),
        ContentLength::TooLarge => Err(HeadError::BadContentLength),
    }
}

fn put_headers(out: &mut Vec<u8>, headers: &[(String, String)]) {
    for (name, value) in headers {
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\r\n");
}

/// Serialized size of `headers` and the blank line after them.
fn headers_size(headers: &[(String, String)]) -> usize {
    headers
        .iter()
        .map(|(n, v)| n.len() + v.len() + 4)
        .sum::<usize>()
        + 2
}

/// Serialized size of a body, chunk framing included (at most 16 hex digits
/// and two CRLFs per chunk).
fn body_size(body: &Body) -> usize {
    match body {
        Body::Empty => 0,
        Body::Full(bytes) => bytes.len(),
        Body::Chunked(parts) => parts.iter().map(|p| p.len() + 20).sum::<usize>() + 5,
    }
}

/// The longest chunk-size line the decoder accepts, extensions included and
/// the CRLF excluded. Real senders use a few hex digits; the bound stops a
/// peer from feeding an endless extension.
pub const MAX_CHUNK_LINE: usize = 4096;

/// The most trailer-section bytes the decoder accepts (CRLFs included).
pub const MAX_TRAILER_SECTION: usize = 64 * 1024;

/// What one [`ChunkedDecoder::feed`] call did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    /// Input bytes used. This is all of the input unless `done`, in which
    /// case the bytes after the body's final CRLF are left for the caller.
    pub consumed: usize,
    /// The last chunk and trailer section have been read.
    pub done: bool,
}

/// Why a chunked body was refused. Once returned, every later `feed` returns
/// the same error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChunkError {
    /// A chunk-size line that does not start with a hex digit, or has a byte
    /// that is not allowed after it.
    BadSizeLine,
    /// A chunk size larger than `u64::MAX`.
    SizeOverflow,
    /// A chunk-size line longer than [`MAX_CHUNK_LINE`].
    LineTooLong,
    /// A missing CRLF after chunk data, or a bare LF or CR where a line ends.
    BadLineEnding,
    /// A trailer line that is not `name: value`.
    BadTrailer,
    /// A trailer section longer than [`MAX_TRAILER_SECTION`].
    TrailersTooLarge,
}

impl fmt::Display for ChunkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ChunkError::BadSizeLine => "invalid chunk-size line",
            ChunkError::SizeOverflow => "chunk size overflows",
            ChunkError::LineTooLong => "chunk-size line too long",
            ChunkError::BadLineEnding => "chunk line not ended by CRLF",
            ChunkError::BadTrailer => "invalid trailer field",
            ChunkError::TrailersTooLarge => "trailer section too large",
        })
    }
}

impl std::error::Error for ChunkError {}

/// An incremental decoder for a `Transfer-Encoding: chunked` body
/// (RFC 9112 §7.1).
///
/// Feed it bytes as they arrive, split anywhere; it appends the chunk data to
/// `out` and keeps its place in between. Chunk extensions are skipped, and
/// trailer fields are checked for shape and discarded.
#[derive(Clone, Debug, Default)]
pub struct ChunkedDecoder {
    state: ChunkState,
    /// Bytes of the current chunk-size line, or of the trailer section.
    line: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ChunkState {
    /// Before the first hex digit of a chunk size.
    #[default]
    Start,
    /// In the hex digits of a chunk size.
    Size {
        size: u64,
    },
    /// After whitespace following the size: only more whitespace, `;` or CR.
    SizeSpace {
        size: u64,
    },
    Extension {
        size: u64,
    },
    SizeLf {
        size: u64,
    },
    Data {
        left: u64,
    },
    DataCr,
    DataLf,
    /// At the start of a trailer line, or of the blank line that ends the body.
    TrailerStart,
    TrailerName,
    TrailerValue,
    TrailerLf,
    EndLf,
    Done,
    Failed(ChunkError),
}

impl ChunkedDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the whole body has been read.
    pub fn is_done(&self) -> bool {
        self.state == ChunkState::Done
    }

    /// Decode as much of `input` as possible, appending chunk data to `out`.
    ///
    /// Stops at the end of the body, reporting how much input it used; the
    /// rest belongs to whatever follows the body. After that, `feed` uses
    /// nothing and reports `done` again.
    pub fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<Progress, ChunkError> {
        if let ChunkState::Failed(err) = self.state {
            return Err(err);
        }
        let mut pos = 0;
        while pos < input.len() && self.state != ChunkState::Done {
            if let ChunkState::Data { left } = self.state {
                let rest = &input[pos..];
                let take = usize::try_from(left).map_or(rest.len(), |left| left.min(rest.len()));
                out.extend_from_slice(&rest[..take]);
                pos += take;
                let left = left - take as u64;
                self.state = if left == 0 {
                    ChunkState::DataCr
                } else {
                    ChunkState::Data { left }
                };
                continue;
            }
            match self.step(input[pos]) {
                Ok(next) => self.state = next,
                Err(err) => {
                    self.state = ChunkState::Failed(err);
                    return Err(err);
                }
            }
            pos += 1;
        }
        Ok(Progress {
            consumed: pos,
            done: self.state == ChunkState::Done,
        })
    }

    /// One byte of framing (anything but chunk data). Returns the next state.
    fn step(&mut self, b: u8) -> Result<ChunkState, ChunkError> {
        use ChunkState as S;
        match self.state {
            S::Start | S::Size { .. } | S::SizeSpace { .. } | S::Extension { .. } if b != b'\r' => {
                self.line += 1;
                if self.line > MAX_CHUNK_LINE {
                    return Err(ChunkError::LineTooLong);
                }
            }
            S::TrailerStart | S::TrailerName | S::TrailerValue | S::TrailerLf | S::EndLf => {
                self.line += 1;
                if self.line > MAX_TRAILER_SECTION {
                    return Err(ChunkError::TrailersTooLarge);
                }
            }
            _ => {}
        }
        let size_so_far = match self.state {
            S::Start => Some(0),
            S::Size { size } => Some(size),
            _ => None,
        };
        if let (Some(size), Some(digit)) = (size_so_far, hex_digit(b)) {
            // Leading zeros never overflow; a significant 17th digit does.
            if size > u64::MAX >> 4 {
                return Err(ChunkError::SizeOverflow);
            }
            return Ok(S::Size {
                size: size << 4 | digit,
            });
        }
        match (self.state, b) {
            (S::Start, _) => Err(ChunkError::BadSizeLine),
            // Whitespace before an extension (RFC 9112's BWS), and, like
            // hyper, before the CRLF.
            (S::Size { size } | S::SizeSpace { size }, b' ' | b'\t') => Ok(S::SizeSpace { size }),
            (S::Size { size } | S::SizeSpace { size }, b';') => Ok(S::Extension { size }),
            (S::Size { size } | S::SizeSpace { size } | S::Extension { size }, b'\r') => {
                Ok(S::SizeLf { size })
            }
            (S::Extension { size }, _) if is_field_byte(b) => Ok(S::Extension { size }),
            (S::Size { .. } | S::SizeSpace { .. } | S::Extension { .. }, b'\n') => {
                Err(ChunkError::BadLineEnding)
            }
            (S::Size { .. } | S::SizeSpace { .. } | S::Extension { .. }, _) => {
                Err(ChunkError::BadSizeLine)
            }
            (S::SizeLf { size: 0 }, b'\n') => {
                self.line = 0;
                Ok(S::TrailerStart)
            }
            (S::SizeLf { size }, b'\n') => Ok(S::Data { left: size }),
            (S::DataCr, b'\r') => Ok(S::DataLf),
            (S::DataLf, b'\n') => {
                self.line = 0;
                Ok(S::Start)
            }
            (S::SizeLf { .. } | S::DataCr | S::DataLf, _) => Err(ChunkError::BadLineEnding),
            (S::TrailerStart, b'\r') => Ok(S::EndLf),
            (S::TrailerStart, _) if is_tchar(b) => Ok(S::TrailerName),
            (S::TrailerName, _) if is_tchar(b) => Ok(S::TrailerName),
            (S::TrailerName, b':') => Ok(S::TrailerValue),
            (S::TrailerValue, b'\r') => Ok(S::TrailerLf),
            (S::TrailerValue, _) if is_field_byte(b) => Ok(S::TrailerValue),
            (S::TrailerStart | S::TrailerName | S::TrailerValue, b'\n') => {
                Err(ChunkError::BadLineEnding)
            }
            (S::TrailerStart | S::TrailerName | S::TrailerValue, _) => Err(ChunkError::BadTrailer),
            (S::TrailerLf, b'\n') => Ok(S::TrailerStart),
            (S::EndLf, b'\n') => Ok(S::Done),
            (S::TrailerLf | S::EndLf, _) => Err(ChunkError::BadLineEnding),
            // `feed` never steps these: data is copied in bulk, and it stops
            // at `Done` and returns early on `Failed`.
            (S::Data { .. } | S::Done | S::Failed(_), _) => Ok(self.state),
        }
    }
}

fn hex_digit(b: u8) -> Option<u64> {
    char::from(b).to_digit(16).map(u64::from)
}

/// RFC 9110 `tchar`: the bytes of a field name.
fn is_tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// HTAB, visible ASCII, space and obs-text: what a field value (or a chunk
/// extension, quoted strings included) may hold.
fn is_field_byte(b: u8) -> bool {
    b == b'\t' || (b' '..=b'~').contains(&b) || b >= 0x80
}
