//! Tests for `steploop::http1::codec`.
//!
//! The head parsers must agree with the async readers they replace (jig at
//! `ca1edfd`) on every well-formed head. `old` below is a verbatim copy of
//! those readers' parsing logic with the sockets removed, and the tests
//! compare against it: on a hand-written table, on random well-formed heads,
//! and on random mutations of them (whenever the new parser accepts a
//! mutant, it must agree with the old one).
//!
//! The head parsers are stateless, so feeding a head in pieces means calling
//! them on each growing prefix. The split tests do exactly what a planner
//! does: append a piece, parse the whole buffer, repeat.

use std::fmt::Debug;

use steploop::http1::codec::{
    BodyLength, ChunkError, ChunkedDecoder, Framing, HeadError, Limits, MAX_CHUNK_LINE,
    MAX_TRAILER_SECTION, Progress, RequestHead, ResponseHead, parse_request_head,
    parse_response_head, reparse, write_request_head, write_response,
};
use steploop::http1::{Body, Response};

// ---------------------------------------------------------------------------
// The reference: jig's parsing and writing at ca1edfd.
// ---------------------------------------------------------------------------

mod old {
    //! Copied from `jig-server/src/server.rs` and `jig-record/src/proxy.rs`
    //! at `ca1edfd`, with socket reads and writes replaced by a buffer. Do not
    //! "fix" anything here: it is the reference.

    pub fn find_header_end(buf: &[u8]) -> Option<usize> {
        buf.windows(4).position(|w| w == b"\r\n\r\n")
    }

    /// jig-server's `read_request`: method, path, Content-Length, head end.
    pub fn server_read_request(buf: &[u8]) -> Option<(String, String, usize, usize)> {
        let header_end = find_header_end(buf)?;
        let header_text = String::from_utf8_lossy(&buf[..header_end]);
        let mut lines = header_text.split("\r\n");

        let request_line = lines.next().unwrap_or("");
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let raw_target = parts.next().unwrap_or("/");
        let path = raw_target.split('?').next().unwrap_or("/").to_string();

        let mut content_length = 0usize;
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                if name.trim().eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
            }
        }
        Some((method, path, content_length, header_end + 4))
    }

    pub struct ClientRequest {
        pub method: String,
        pub target: String,
        pub headers: Vec<(String, String)>,
        pub content_length: usize,
        pub head_len: usize,
    }

    /// jig-record's `read_client_request`, minus the body read.
    pub fn proxy_read_client_request(buf: &[u8]) -> Option<ClientRequest> {
        let header_end = find_header_end(buf)?;
        let header_text = String::from_utf8_lossy(&buf[..header_end]).into_owned();
        let mut lines = header_text.split("\r\n");

        let request_line = lines.next().unwrap_or("");
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let target = parts.next().unwrap_or("/").to_string();

        let mut headers = Vec::new();
        let mut content_length = 0usize;
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                let name = name.trim().to_string();
                let value = value.trim().to_string();
                if name.eq_ignore_ascii_case("content-length") {
                    content_length = value.parse().unwrap_or(0);
                }
                headers.push((name, value));
            }
        }
        Some(ClientRequest {
            method,
            target,
            headers,
            content_length,
            head_len: header_end + 4,
        })
    }

    /// Status, headers, head length.
    pub type ResponseHead = (u16, Vec<(String, String)>, usize);

    /// jig-record's `parse_response_head`, applied as `forward` applied it.
    pub fn parse_response(buf: &[u8]) -> Option<ResponseHead> {
        let header_end = find_header_end(buf)?;
        let (status, headers) = parse_response_head(&buf[..header_end]);
        Some((status, headers, header_end + 4))
    }

    fn parse_response_head(head: &[u8]) -> (u16, Vec<(String, String)>) {
        let text = String::from_utf8_lossy(head);
        let mut lines = text.split("\r\n");
        let status_line = lines.next().unwrap_or("");
        let status = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let mut headers = Vec::new();
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                headers.push((name.trim().to_string(), value.trim().to_string()));
            }
        }
        (status, headers)
    }

    /// jig-server's `write_sse_response`: its `write_all` calls, in order.
    pub fn write_sse_response(body: &str) -> Vec<u8> {
        let mut stream = Vec::new();
        let headers = "HTTP/1.1 200 OK\r\n\
             Content-Type: text/event-stream\r\n\
             Cache-Control: no-cache\r\n\
             Transfer-Encoding: chunked\r\n\
             Connection: close\r\n\
             \r\n";
        stream.extend_from_slice(headers.as_bytes());
        let chunk_header = format!("{:x}\r\n", body.len());
        stream.extend_from_slice(chunk_header.as_bytes());
        stream.extend_from_slice(body.as_bytes());
        stream.extend_from_slice(b"\r\n0\r\n\r\n");
        stream
    }

    /// jig-server's `write_http_error`, given what it computed from the
    /// `HttpError` (status, reason, content type, extra headers, body).
    pub fn write_http_error(
        status: u16,
        reason: &str,
        content_type: &str,
        headers: &[(String, String)],
        body: &str,
    ) -> Vec<u8> {
        let mut stream = Vec::new();
        let mut response = format!(
            "HTTP/1.1 {} {reason}\r\n\
             Content-Type: {content_type}\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n",
            status,
            body.len()
        );
        for (name, value) in headers {
            if name.eq_ignore_ascii_case("content-type")
                || name.eq_ignore_ascii_case("content-length")
                || name.eq_ignore_ascii_case("connection")
            {
                continue;
            }
            response.push_str(name);
            response.push_str(": ");
            response.push_str(value);
            response.push_str("\r\n");
        }
        response.push_str("\r\n");
        stream.extend_from_slice(response.as_bytes());
        stream.extend_from_slice(body.as_bytes());
        stream
    }

    /// jig-server's `write_not_found`.
    pub fn write_not_found() -> Vec<u8> {
        let response = "HTTP/1.1 404 Not Found\r\n\
             Content-Length: 0\r\n\
             Connection: close\r\n\
             \r\n";
        response.as_bytes().to_vec()
    }

    /// jig-record's `answer_preflight`.
    pub fn write_preflight() -> Vec<u8> {
        b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
    }

    /// jig-record's `build_upstream_request_head`.
    pub fn build_upstream_request_head(
        method: &str,
        target: &str,
        headers: &[(String, String)],
        upstream_host: &str,
    ) -> String {
        let mut head = format!("{} {} HTTP/1.1\r\n", method, target);
        let mut saw_accept_encoding = false;
        for (name, value) in headers {
            if name.eq_ignore_ascii_case("host") {
                continue;
            }
            if name.eq_ignore_ascii_case("accept-encoding") {
                head.push_str("Accept-Encoding: identity\r\n");
                saw_accept_encoding = true;
                continue;
            }
            head.push_str(&format!("{}: {}\r\n", name, value));
        }
        head.push_str(&format!("Host: {}\r\n", upstream_host));
        if !saw_accept_encoding {
            head.push_str("Accept-Encoding: identity\r\n");
        }
        head.push_str("Connection: close\r\n\r\n");
        head
    }
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

/// splitmix64: a small deterministic PRNG, so failures reproduce.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n`; `n` must be positive.
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }

    /// Sorted, distinct cut points strictly inside `0..len`.
    fn cuts(&mut self, len: usize) -> Vec<usize> {
        if len < 2 {
            return Vec::new();
        }
        let mut cuts: Vec<usize> = (0..self.below(8) + 1)
            .map(|_| 1 + self.below(len - 1))
            .collect();
        cuts.sort_unstable();
        cuts.dedup();
        cuts
    }
}

fn owned(headers: &[(&str, &str)]) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(n, v)| (n.to_string(), v.to_string()))
        .collect()
}

fn req(buf: &[u8]) -> Result<Option<RequestHead>, HeadError> {
    parse_request_head(buf, &Limits::default())
}

fn res(buf: &[u8]) -> Result<Option<ResponseHead>, HeadError> {
    parse_response_head(buf, &Limits::default())
}

/// Feeds `input` as a planner would: append the piece ending at each cut
/// (then the rest), parse the whole buffer after each. Before the buffer
/// holds `head_len` bytes the parser must say "incomplete"; from then on it
/// must return `expected`.
fn feed_head<T: PartialEq + Debug>(
    input: &[u8],
    cuts: &[usize],
    head_len: usize,
    expected: &T,
    parse: &impl Fn(&[u8]) -> Result<Option<T>, HeadError>,
) {
    let mut buf = Vec::new();
    let mut start = 0;
    for end in cuts.iter().copied().chain([input.len()]) {
        buf.extend_from_slice(&input[start..end]);
        start = end;
        match parse(&buf) {
            Ok(None) => assert!(
                buf.len() < head_len,
                "incomplete with {} bytes buffered, but the head is {head_len}: {:?}",
                buf.len(),
                String::from_utf8_lossy(input)
            ),
            Ok(Some(head)) => {
                assert!(buf.len() >= head_len, "complete before the head ended");
                assert_eq!(&head, expected, "cuts {cuts:?}");
                return;
            }
            Err(err) => panic!(
                "{err} after cuts {cuts:?}: {:?}",
                String::from_utf8_lossy(input)
            ),
        }
    }
    panic!("never completed: {:?}", String::from_utf8_lossy(input));
}

/// Every two-piece split, byte at a time, and some random splits.
fn check_head_splits<T: PartialEq + Debug>(
    input: &[u8],
    head_len: usize,
    expected: &T,
    parse: impl Fn(&[u8]) -> Result<Option<T>, HeadError>,
    rng: &mut Rng,
) {
    for cut in 0..=input.len() {
        feed_head(input, &[cut], head_len, expected, &parse);
    }
    let bytewise: Vec<usize> = (1..input.len()).collect();
    feed_head(input, &bytewise, head_len, expected, &parse);
    for _ in 0..20 {
        let cuts = rng.cuts(input.len());
        feed_head(input, &cuts, head_len, expected, &parse);
    }
}

/// A malformed head is refused as soon as the offending byte is buffered and
/// with the same error from then on; before that it is merely incomplete.
fn check_head_error<T: Debug>(
    input: &[u8],
    expected: HeadError,
    parse: impl Fn(&[u8]) -> Result<Option<T>, HeadError>,
) {
    assert_eq!(
        parse(input).err(),
        Some(expected),
        "{:?}",
        String::from_utf8_lossy(input)
    );
    let mut failed = false;
    for end in 0..=input.len() {
        match parse(&input[..end]) {
            Ok(None) => assert!(!failed, "incomplete again after an error"),
            Ok(Some(head)) => panic!("prefix of {end} bytes parsed as {head:?}"),
            Err(err) => {
                assert_eq!(err, expected, "prefix of {end} bytes");
                failed = true;
            }
        }
    }
}

/// The new parser's view of a request, next to what jig-server and jig-record
/// computed from the same bytes.
fn assert_request_matches_old(buf: &[u8], head: &RequestHead) {
    let proxy = old::proxy_read_client_request(buf).expect("old proxy found no head");
    let (method, path, content_length, head_len) =
        old::server_read_request(buf).expect("old server found no head");
    let context = String::from_utf8_lossy(buf);
    assert_eq!(head.method, proxy.method, "{context:?}");
    assert_eq!(head.target, proxy.target, "{context:?}");
    assert_eq!(head.headers, proxy.headers, "{context:?}");
    assert_eq!(head.body_len(), proxy.content_length, "{context:?}");
    assert_eq!(head.head_len, proxy.head_len, "{context:?}");
    let request = head.clone().into_request(Vec::new());
    assert_eq!(request.method, method, "{context:?}");
    assert_eq!(request.path(), path, "{context:?}");
    assert_eq!(head.body_len(), content_length, "{context:?}");
    assert_eq!(head.head_len, head_len, "{context:?}");
}

fn assert_response_matches_old(buf: &[u8], head: &ResponseHead) {
    let (status, headers, head_len) = old::parse_response(buf).expect("old found no head");
    let context = String::from_utf8_lossy(buf);
    assert_eq!(head.status, status, "{context:?}");
    assert_eq!(head.headers, headers, "{context:?}");
    assert_eq!(head.head_len, head_len, "{context:?}");
}

// ---------------------------------------------------------------------------
// Request heads: equivalence with the old readers.
// ---------------------------------------------------------------------------

struct RequestCase {
    raw: &'static [u8],
    method: &'static str,
    target: &'static str,
    path: &'static str,
    headers: &'static [(&'static str, &'static str)],
    content_length: usize,
}

/// Well-formed requests, with what the old readers computed for each. The
/// expectations were derived by reading the old code, and the test checks
/// them against the copy of it in `old` as well as against the new parser.
const REQUESTS: &[RequestCase] = &[
    RequestCase {
        raw: b"GET / HTTP/1.1\r\n\r\n",
        method: "GET",
        target: "/",
        path: "/",
        headers: &[],
        content_length: 0,
    },
    RequestCase {
        raw: b"POST /chat/completions HTTP/1.1\r\nHost: 127.0.0.1:5050\r\n\
               Content-Type: application/json\r\nContent-Length: 2\r\n\r\n{}",
        method: "POST",
        target: "/chat/completions",
        path: "/chat/completions",
        headers: &[
            ("Host", "127.0.0.1:5050"),
            ("Content-Type", "application/json"),
            ("Content-Length", "2"),
        ],
        content_length: 2,
    },
    RequestCase {
        raw: b"POST /v1/messages?beta=true HTTP/1.1\r\nhost: localhost\r\n\
               anthropic-version: 2023-06-01\r\ncontent-length: 5\r\n\r\nhello",
        method: "POST",
        target: "/v1/messages?beta=true",
        path: "/v1/messages",
        headers: &[
            ("host", "localhost"),
            ("anthropic-version", "2023-06-01"),
            ("content-length", "5"),
        ],
        content_length: 5,
    },
    RequestCase {
        raw: b"GET /x HTTP/1.1\r\nX-A:   spaced value \t\r\nX-B:v\r\nX-Empty:\r\n\
               X-Blank: \t \r\n\r\n",
        method: "GET",
        target: "/x",
        path: "/x",
        headers: &[
            ("X-A", "spaced value"),
            ("X-B", "v"),
            ("X-Empty", ""),
            ("X-Blank", ""),
        ],
        content_length: 0,
    },
    RequestCase {
        // Case kept, repeats kept in order, and two Content-Lengths that
        // agree (the old reader took the last).
        raw: b"POST /backend-api/codex/responses HTTP/1.1\r\ncontent-LENGTH: 3\r\n\
               Accept: a\r\nAccept: b\r\nContent-Length: 003\r\n\r\nabc",
        method: "POST",
        target: "/backend-api/codex/responses",
        path: "/backend-api/codex/responses",
        headers: &[
            ("content-LENGTH", "3"),
            ("Accept", "a"),
            ("Accept", "b"),
            ("Content-Length", "003"),
        ],
        content_length: 3,
    },
    RequestCase {
        raw: b"GET /p HTTP/1.1\r\nAuthorization: Bearer a:b\r\nReferer: http://x:1/y\r\n\r\n",
        method: "GET",
        target: "/p",
        path: "/p",
        headers: &[("Authorization", "Bearer a:b"), ("Referer", "http://x:1/y")],
        content_length: 0,
    },
    RequestCase {
        raw: b"HEAD / HTTP/1.0\r\nUser-Agent: probe/1.0\r\n\r\n",
        method: "HEAD",
        target: "/",
        path: "/",
        headers: &[("User-Agent", "probe/1.0")],
        content_length: 0,
    },
    RequestCase {
        raw: b"GET http://api.openai.com/v1/models?limit=1 HTTP/1.1\r\n\r\n",
        method: "GET",
        target: "http://api.openai.com/v1/models?limit=1",
        path: "http://api.openai.com/v1/models",
        headers: &[],
        content_length: 0,
    },
    RequestCase {
        raw: b"OPTIONS * HTTP/1.1\r\nHost: x\r\n\r\n",
        method: "OPTIONS",
        target: "*",
        path: "*",
        headers: &[("Host", "x")],
        content_length: 0,
    },
    RequestCase {
        raw: b"M-SEARCH /a%20b/c;d=e?f=g&h=[i]#frag HTTP/1.1\r\n\r\n",
        method: "M-SEARCH",
        target: "/a%20b/c;d=e?f=g&h=[i]#frag",
        path: "/a%20b/c;d=e",
        headers: &[],
        content_length: 0,
    },
    RequestCase {
        raw: b"GET /?q HTTP/1.1\r\n\r\n",
        method: "GET",
        target: "/?q",
        path: "/",
        headers: &[],
        content_length: 0,
    },
    RequestCase {
        // obs-text: UTF-8, invalid UTF-8 (lossy), and non-ASCII whitespace,
        // which the old `str::trim` removed.
        raw: b"GET / HTTP/1.1\r\nX-Name: h\xc3\xa9llo\r\nX-Bad: a\xffb\r\n\
               X-Nbsp: \xc2\xa0pad\xc2\xa0\r\nX-Ideo: \xe3\x80\x80w\r\nX-Tab: a\tb\r\n\r\n",
        method: "GET",
        target: "/",
        path: "/",
        headers: &[
            ("X-Name", "h\u{e9}llo"),
            ("X-Bad", "a\u{fffd}b"),
            ("X-Nbsp", "pad"),
            ("X-Ideo", "w"),
            ("X-Tab", "a\tb"),
        ],
        content_length: 0,
    },
    RequestCase {
        raw: b"POST /x HTTP/1.1\r\nContent-Length: 0\r\n\r\n",
        method: "POST",
        target: "/x",
        path: "/x",
        headers: &[("Content-Length", "0")],
        content_length: 0,
    },
    RequestCase {
        // Bytes past the declared body belong to no one; the head is the same.
        raw: b"POST /x HTTP/1.1\r\nContent-Length:  1 \r\n\r\nab",
        method: "POST",
        target: "/x",
        path: "/x",
        headers: &[("Content-Length", "1")],
        content_length: 1,
    },
    RequestCase {
        // A body that itself contains CRLF CRLF.
        raw: b"PUT /f HTTP/1.1\r\nContent-Length: 8\r\n\r\n\r\n\r\nxy\r\n",
        method: "PUT",
        target: "/f",
        path: "/f",
        headers: &[("Content-Length", "8")],
        content_length: 8,
    },
];

#[test]
fn request_table_matches_old_readers() {
    for case in REQUESTS {
        let context = String::from_utf8_lossy(case.raw);

        // The table says what the old code computes...
        let proxy = old::proxy_read_client_request(case.raw).unwrap();
        let server = old::server_read_request(case.raw).unwrap();
        assert_eq!(proxy.method, case.method, "{context:?}");
        assert_eq!(proxy.target, case.target, "{context:?}");
        assert_eq!(proxy.headers, owned(case.headers), "{context:?}");
        assert_eq!(proxy.content_length, case.content_length, "{context:?}");
        assert_eq!(server.1, case.path, "{context:?}");

        // ...and the new parser computes the same.
        let head = req(case.raw).unwrap().unwrap();
        assert_eq!(head.method, case.method, "{context:?}");
        assert_eq!(head.target, case.target, "{context:?}");
        assert_eq!(head.headers, owned(case.headers), "{context:?}");
        assert_eq!(head.body_len(), case.content_length, "{context:?}");
        assert_eq!(head.clone().into_request(Vec::new()).path(), case.path);
        assert_request_matches_old(case.raw, &head);
    }
}

#[test]
fn request_body_length_distinguishes_absent_from_zero() {
    let absent = req(b"GET / HTTP/1.1\r\n\r\n").unwrap().unwrap();
    assert_eq!(absent.body, BodyLength::None);
    let zero = req(b"GET / HTTP/1.1\r\nContent-Length: 0\r\n\r\n")
        .unwrap()
        .unwrap();
    assert_eq!(zero.body, BodyLength::Length(0));
    let head = b"POST / HTTP/1.1\r\nContent-Length: 3\r\n\r\n";
    let request = req(&[&head[..], b"abc"].concat()).unwrap().unwrap();
    assert_eq!(request.body, BodyLength::Length(3));
    assert_eq!(request.head_len, head.len());
    let request = request.into_request(b"abc".to_vec());
    assert_eq!(request.header("content-length"), Some("3"));
    assert_eq!(request.body, b"abc");
}

#[test]
fn request_table_at_every_split() {
    let mut rng = Rng(1);
    for case in REQUESTS {
        let expected = req(case.raw).unwrap().unwrap();
        check_head_splits(case.raw, expected.head_len, &expected, req, &mut rng);
    }
}

/// The deliberate difference: empty lines before the request line are
/// skipped (RFC 9112 §2.2), where the old readers took the first one as an
/// empty request line.
#[test]
fn request_leading_empty_lines_are_skipped() {
    let raw = b"\r\n\r\nGET /v1/messages HTTP/1.1\r\nHost: x\r\n\r\n";
    let head = req(raw).unwrap().unwrap();
    assert_eq!(head.method, "GET");
    assert_eq!(head.target, "/v1/messages");
    assert_eq!(head.headers, owned(&[("Host", "x")]));
    assert_eq!(head.head_len, raw.len());

    let (method, path, _, _) = old::server_read_request(raw).unwrap();
    assert_eq!((method.as_str(), path.as_str()), ("", "/"));
}

// Random well-formed requests.

const KNOWN_NAMES: &[&str] = &[
    "Host",
    "Content-Type",
    "Accept",
    "User-Agent",
    "Authorization",
    "x-api-key",
    "anthropic-version",
    "Accept-Encoding",
    "Connection",
    "openai-beta",
];

const TCHARS: &[u8] =
    b"!#$%&'*+-.^_`|~0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// Sequences that stress lossy decoding and Unicode trimming.
const VALUE_EXTRAS: &[&[u8]] = &[
    b"\xc3\xa9",     // é
    b"\xc2\xa0",     // NBSP (Unicode whitespace)
    b"\xc2\x85",     // NEL (Unicode whitespace)
    b"\xe3\x80\x80", // IDEOGRAPHIC SPACE
    b"\xe2\x80\xa8", // LINE SEPARATOR
    b"\xff",         // invalid UTF-8
    b"\xe2\x82",     // truncated UTF-8
    b"\t",
    b" ",
    b":",
];

fn random_token(rng: &mut Rng, max: usize) -> Vec<u8> {
    (0..rng.below(max) + 1).map(|_| rng.pick(TCHARS)).collect()
}

const OWS: &[&[u8]] = &[b"", b" ", b"  ", b"\t", b" \t "];

fn random_ows(rng: &mut Rng) -> &'static [u8] {
    rng.pick(OWS)
}

fn random_value(rng: &mut Rng) -> Vec<u8> {
    let mut value = Vec::new();
    for _ in 0..rng.below(20) {
        if rng.chance(20) {
            value.extend_from_slice(rng.pick(VALUE_EXTRAS));
        } else {
            value.push(b'!' + rng.below(94) as u8);
        }
    }
    value
}

fn random_header_name(rng: &mut Rng) -> Vec<u8> {
    loop {
        let name = if rng.chance(40) {
            rng.pick(KNOWN_NAMES).as_bytes().to_vec()
        } else {
            random_token(rng, 16)
        };
        let lower = name.to_ascii_lowercase();
        if lower != b"content-length" && lower != b"transfer-encoding" {
            return name;
        }
    }
}

fn random_header_lines(rng: &mut Rng, count: usize, raw: &mut Vec<u8>) {
    for _ in 0..count {
        raw.extend_from_slice(&random_header_name(rng));
        raw.push(b':');
        raw.extend_from_slice(random_ows(rng));
        raw.extend_from_slice(&random_value(rng));
        raw.extend_from_slice(random_ows(rng));
        raw.extend_from_slice(b"\r\n");
    }
}

/// A Content-Length field line for `n`, possibly zero-padded and padded
/// with whitespace, in a random case.
fn content_length_line(rng: &mut Rng, n: usize) -> Vec<u8> {
    let name = rng.pick(&["Content-Length", "content-length", "CONTENT-length"]);
    let zeros = "0".repeat(rng.below(3));
    let mut line = format!("{name}:").into_bytes();
    line.extend_from_slice(random_ows(rng));
    line.extend_from_slice(format!("{zeros}{n}").as_bytes());
    line.extend_from_slice(random_ows(rng));
    line.extend_from_slice(b"\r\n");
    line
}

fn random_request(rng: &mut Rng) -> Vec<u8> {
    let mut raw = Vec::new();
    if rng.chance(70) {
        raw.extend_from_slice(
            rng.pick(&["GET", "POST", "HEAD", "PUT", "DELETE", "OPTIONS", "PATCH"])
                .as_bytes(),
        );
    } else {
        raw.extend_from_slice(&random_token(rng, 8));
    }
    raw.push(b' ');
    match rng.below(10) {
        0 => raw.push(b'*'),
        1 => raw.extend_from_slice(b"http://api.example.com:443/v1"),
        _ => raw.push(b'/'),
    }
    for _ in 0..rng.below(30) {
        // Any visible ASCII, '?' and '#' included.
        raw.push(b'!' + rng.below(94) as u8);
    }
    raw.extend_from_slice(if rng.chance(80) {
        b" HTTP/1.1\r\n"
    } else {
        b" HTTP/1.0\r\n"
    });

    let body_len = rng.below(40);
    let mut lines = Vec::new();
    let count = rng.below(12);
    random_header_lines(rng, count, &mut lines);
    if rng.chance(60) {
        // Insert one or two agreeing Content-Lengths among the fields.
        for _ in 0..rng.below(2) + 1 {
            let line = content_length_line(rng, body_len);
            let at = line_boundary(&lines, rng);
            lines.splice(at..at, line);
        }
    }
    raw.extend_from_slice(&lines);
    raw.extend_from_slice(b"\r\n");
    for _ in 0..body_len {
        raw.push(rng.next() as u8);
    }
    raw
}

/// A random offset in `lines` that starts a line.
fn line_boundary(lines: &[u8], rng: &mut Rng) -> usize {
    let mut starts = vec![0];
    starts.extend(
        lines
            .windows(2)
            .enumerate()
            .filter(|(_, w)| w == b"\r\n")
            .map(|(i, _)| i + 2),
    );
    rng.pick(&starts)
}

#[test]
fn random_requests_match_old_readers() {
    let mut rng = Rng(2);
    for i in 0..3000 {
        let raw = random_request(&mut rng);
        let head = match req(&raw) {
            Ok(Some(head)) => head,
            other => panic!("{other:?} for {:?}", String::from_utf8_lossy(&raw)),
        };
        assert_request_matches_old(&raw, &head);
        if i < 100 {
            check_head_splits(&raw, head.head_len, &head, req, &mut rng);
        }
    }
}

/// Structural bytes, bytes the grammar forbids, and non-ASCII whitespace.
const MUTANT_BYTES: &[u8] = &[
    b' ', b'\t', b':', b'\r', b'\n', 0x00, 0x7f, 0xa0, 0xc2, 0x85, 0xff, b'a', b'+', b',', b'-',
    b'0', b'5', b'?', b'/', b';', b'"',
];

/// Applies one to three random edits, almost always inside the head: body
/// bytes don't reach the head parsers.
fn mutate(rng: &mut Rng, raw: &mut Vec<u8>) {
    let head_len = old::find_header_end(raw).map_or(raw.len(), |end| end + 4);
    for _ in 0..rng.below(3) + 1 {
        let span = if rng.chance(95) { head_len } else { raw.len() };
        let at = rng.below(span.min(raw.len()) + 1);
        let byte = rng.pick(MUTANT_BYTES);
        match rng.below(4) {
            0 if at < raw.len() => raw[at] = byte,
            1 if at < raw.len() => {
                raw.remove(at);
            }
            // Whole UTF-8 sequences, e.g. Unicode whitespace.
            2 => {
                let extra = rng.pick(VALUE_EXTRAS);
                raw.splice(at..at, extra.iter().copied());
            }
            _ => raw.insert(at, byte),
        }
    }
}

/// Whatever the new parser accepts, it reads exactly as the old readers did.
/// The one exception is the leading empty line, tested above.
#[test]
fn accepted_request_mutants_match_old_readers() {
    let mut rng = Rng(3);
    let (mut accepted, mut refused) = (0, 0);
    for _ in 0..30_000 {
        let mut raw = random_request(&mut rng);
        mutate(&mut rng, &mut raw);
        match req(&raw) {
            Ok(Some(head)) if !raw.starts_with(b"\r\n") => {
                assert_request_matches_old(&raw, &head);
                accepted += 1;
            }
            Ok(_) => {}
            Err(_) => refused += 1,
        }
    }
    assert!(accepted > 3000, "only {accepted} mutants accepted");
    assert!(refused > 3000, "only {refused} mutants refused");
}

// ---------------------------------------------------------------------------
// Request heads: errors and limits.
// ---------------------------------------------------------------------------

#[test]
fn malformed_requests_get_400() {
    let cases: &[&[u8]] = &[
        b"GET  / HTTP/1.1\r\n\r\n",
        b"GET / HTTP/2.0\r\n\r\n",
        b"GET / HTTP/1.1 \r\n\r\n",
        b"GET /\r\n\r\n",
        b"G(T / HTTP/1.1\r\n\r\n",
        b"GET / HTTP/1.1\r\nBad Header: x\r\n\r\n",
        b"GET / HTTP/1.1\r\nName : x\r\n\r\n",
        b"GET / HTTP/1.1\r\nNoColon\r\n\r\n",
        b"GET / HTTP/1.1\r\n: empty-name\r\n\r\n",
        b"GET / HTTP/1.1\r\nX: a\rb\r\n\r\n",
        b"GET / HTTP/1.1\r\nX: a\x00b\r\n\r\n",
        b"GET / HTTP/1.1\r\nX: a\x7fb\r\n\r\n",
        b"GET / HTTP/1.1\r\nX: a\r\n folded\r\n\r\n",
        b"GET / HTTP/1.1\r\n X: leading-space\r\n\r\n",
        // Bare LF, anywhere.
        b"GET / HTTP/1.1\nHost: x\n\n",
        b"GET / HTTP/1.1\r\nHost: x\n\r\n",
        b"GET / HTTP/1.1\r\nHost: x\r\n\n",
        b"\nGET / HTTP/1.1\r\n\r\n",
        // The target must be ASCII.
        b"GET /caf\xc3\xa9 HTTP/1.1\r\n\r\n",
        b"GET /a\xc2\xa0b HTTP/1.1\r\n\r\n",
        b"GET /a\xff HTTP/1.1\r\n\r\n",
        b"GET /a\x7f HTTP/1.1\r\n\r\n",
        // TLS spoken to a plain listener.
        b"\x16\x03\x01\x02\x00\x01\x00\x01\xfc\x03\x03",
    ];
    for raw in cases {
        let err = req(raw).expect_err(&format!("{:?}", String::from_utf8_lossy(raw)));
        assert!(matches!(err, HeadError::Malformed(_)), "{err:?}");
        assert_eq!(err.status(), (400, "Bad Request"));
        check_head_error(raw, err, req);
    }
}

#[test]
fn request_transfer_encoding_gets_501() {
    let cases: &[&[u8]] = &[
        b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
        b"POST / HTTP/1.1\r\ntransfer-encoding: identity\r\nContent-Length: 5\r\n\r\n",
        b"POST / HTTP/1.1\r\nContent-Length: 5\r\nTRANSFER-ENCODING: gzip, chunked\r\n\r\n",
        b"POST / HTTP/1.1\r\nTransfer-Encoding:\r\n\r\n",
    ];
    for raw in cases {
        check_head_error(raw, HeadError::TransferEncoding, req);
    }
    assert_eq!(
        HeadError::TransferEncoding.status(),
        (501, "Not Implemented")
    );
}

#[test]
fn bad_request_content_length_gets_400() {
    for value in [
        "abc", "", "+5", "-1", "5, 5", "5,5", "5 5", "0x5", "5.0", "\u{661}",
    ] {
        let raw = format!("POST / HTTP/1.1\r\nContent-Length: {value}\r\n\r\n");
        check_head_error(raw.as_bytes(), HeadError::BadContentLength, req);
    }
    for raw in [
        &b"POST / HTTP/1.1\r\nContent-Length: 5\r\nContent-Length: 6\r\n\r\n"[..],
        b"POST / HTTP/1.1\r\nContent-Length: 5\r\nContent-Length: x\r\n\r\n",
        b"POST / HTTP/1.1\r\nContent-Length: 99999999999999999999999\r\nContent-Length: 5\r\n\r\n",
    ] {
        check_head_error(raw, HeadError::BadContentLength, req);
    }
    assert_eq!(HeadError::BadContentLength.status(), (400, "Bad Request"));
}

#[test]
fn request_body_limit() {
    let limits = Limits {
        max_body: 10,
        ..Limits::default()
    };
    let parse = |buf: &[u8]| parse_request_head(buf, &limits);
    let head = parse(b"POST / HTTP/1.1\r\nContent-Length: 10\r\n\r\n")
        .unwrap()
        .unwrap();
    assert_eq!(head.body, BodyLength::Length(10));
    check_head_error(
        b"POST / HTTP/1.1\r\nContent-Length: 11\r\n\r\n",
        HeadError::BodyTooLarge,
        parse,
    );

    let max = Limits::default().max_body;
    assert_eq!(max, 64 * 1024 * 1024);
    let at_max = format!("POST / HTTP/1.1\r\nContent-Length: {max}\r\n\r\n");
    assert_eq!(req(at_max.as_bytes()).unwrap().unwrap().body_len(), max);
    let over = format!("POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n", max + 1);
    check_head_error(over.as_bytes(), HeadError::BodyTooLarge, req);
    check_head_error(
        b"POST / HTTP/1.1\r\nContent-Length: 99999999999999999999999\r\n\r\n",
        HeadError::BodyTooLarge,
        req,
    );
    assert_eq!(HeadError::BodyTooLarge.status(), (413, "Content Too Large"));
}

/// A request head of exactly `len` bytes.
fn request_of_len(len: usize) -> Vec<u8> {
    let fixed = b"GET / HTTP/1.1\r\nX-Pad: \r\n\r\n".len();
    let mut raw = b"GET / HTTP/1.1\r\nX-Pad: ".to_vec();
    raw.extend(std::iter::repeat_n(b'p', len - fixed));
    raw.extend_from_slice(b"\r\n\r\n");
    raw
}

#[test]
fn request_head_limit() {
    for max_head in [64, 1000, Limits::default().max_head] {
        let limits = Limits {
            max_head,
            ..Limits::default()
        };
        let parse = |buf: &[u8]| parse_request_head(buf, &limits);

        // Exactly at the limit is fine, even with a body buffered after it.
        let mut raw = request_of_len(max_head);
        assert_eq!(parse(&raw).unwrap().unwrap().head_len, max_head);
        raw.extend_from_slice(b"trailing body bytes");
        assert_eq!(parse(&raw).unwrap().unwrap().head_len, max_head);

        // One byte over is refused as soon as the limit is buffered.
        let raw = request_of_len(max_head + 1);
        assert_eq!(parse(&raw[..max_head - 1]), Ok(None));
        assert_eq!(parse(&raw[..max_head]), Err(HeadError::HeadTooLarge));
        assert_eq!(parse(&raw), Err(HeadError::HeadTooLarge));
        if max_head <= 1000 {
            // Every prefix: quadratic, so only for the small limits.
            check_head_error(&raw, HeadError::HeadTooLarge, parse);
        }
    }
    assert_eq!(Limits::default().max_head, 64 * 1024);
    assert_eq!(HeadError::HeadTooLarge.status(), (413, "Content Too Large"));
}

fn request_with_headers(count: usize) -> Vec<u8> {
    let mut raw = b"GET / HTTP/1.1\r\n".to_vec();
    for i in 0..count {
        raw.extend_from_slice(format!("X-{i}: v\r\n").as_bytes());
    }
    raw.extend_from_slice(b"\r\n");
    raw
}

#[test]
fn request_header_count_limit() {
    let max = Limits::default().max_headers;
    assert_eq!(max, 100);
    assert_eq!(
        req(&request_with_headers(max))
            .unwrap()
            .unwrap()
            .headers
            .len(),
        max
    );
    check_head_error(
        &request_with_headers(max + 1),
        HeadError::TooManyHeaders,
        req,
    );

    let limits = Limits {
        max_headers: 2,
        ..Limits::default()
    };
    let parse = |buf: &[u8]| parse_request_head(buf, &limits);
    assert!(parse(&request_with_headers(2)).unwrap().is_some());
    check_head_error(&request_with_headers(3), HeadError::TooManyHeaders, parse);
    assert_eq!(
        HeadError::TooManyHeaders.status(),
        (413, "Content Too Large")
    );
}

#[test]
fn empty_and_partial_requests_are_incomplete() {
    assert_eq!(req(b""), Ok(None));
    assert_eq!(req(b"\r\n"), Ok(None));
    assert_eq!(
        req(b"POST /chat/completions HTTP/1.1\r\nHost: x\r\n"),
        Ok(None)
    );
    assert_eq!(
        req(b"POST /chat/completions HTTP/1.1\r\nHost: x\r\n\r"),
        Ok(None)
    );
}

// ---------------------------------------------------------------------------
// Response heads.
// ---------------------------------------------------------------------------

struct ResponseCase {
    raw: &'static [u8],
    status: u16,
    reason: &'static str,
    headers: &'static [(&'static str, &'static str)],
    framing: Framing,
}

const RESPONSES: &[ResponseCase] = &[
    ResponseCase {
        raw: b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
               Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n\
               6\r\ndata: \r\n0\r\n\r\n",
        status: 200,
        reason: "OK",
        headers: &[
            ("Content-Type", "text/event-stream"),
            ("Transfer-Encoding", "chunked"),
            ("Connection", "close"),
        ],
        framing: Framing::Chunked,
    },
    ResponseCase {
        raw: b"HTTP/1.1 200 OK\r\ndate: Tue, 29 Sep 2026 10:00:00 GMT\r\n\
               content-type: text/event-stream; charset=utf-8\r\ncache-control: no-cache\r\n\
               set-cookie: a=b; Path=/\r\nset-cookie: c=d\r\ncf-ray: 8c1f2e-LHR\r\n\r\n\
               event: ping\n\n",
        status: 200,
        reason: "OK",
        headers: &[
            ("date", "Tue, 29 Sep 2026 10:00:00 GMT"),
            ("content-type", "text/event-stream; charset=utf-8"),
            ("cache-control", "no-cache"),
            ("set-cookie", "a=b; Path=/"),
            ("set-cookie", "c=d"),
            ("cf-ray", "8c1f2e-LHR"),
        ],
        framing: Framing::UntilEof,
    },
    ResponseCase {
        raw: b"HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\n\
               Content-Length: 16\r\nRetry-After:  3 \r\n\r\n{\"error\":\"slow\"}",
        status: 429,
        reason: "Too Many Requests",
        headers: &[
            ("Content-Type", "application/json"),
            ("Content-Length", "16"),
            ("Retry-After", "3"),
        ],
        framing: Framing::Length(16),
    },
    ResponseCase {
        raw: b"HTTP/1.0 204 No Content\r\nContent-Length: 0\r\n\r\n",
        status: 204,
        reason: "No Content",
        headers: &[("Content-Length", "0")],
        framing: Framing::Empty,
    },
    ResponseCase {
        raw: b"HTTP/1.1 304 Not Modified\r\nContent-Length: 10\r\n\r\n",
        status: 304,
        reason: "Not Modified",
        headers: &[("Content-Length", "10")],
        framing: Framing::Empty,
    },
    ResponseCase {
        raw: b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n",
        status: 101,
        reason: "Switching Protocols",
        headers: &[("Upgrade", "websocket")],
        framing: Framing::Empty,
    },
    ResponseCase {
        raw: b"HTTP/1.1 200\r\nX: y\r\n\r\n",
        status: 200,
        reason: "",
        headers: &[("X", "y")],
        framing: Framing::UntilEof,
    },
    ResponseCase {
        raw: b"HTTP/1.1 404 Not  Found \r\n\r\n",
        status: 404,
        reason: "Not  Found ",
        headers: &[],
        framing: Framing::UntilEof,
    },
    ResponseCase {
        raw: b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip, chunked\r\nContent-Length: 3\r\n\r\n",
        status: 200,
        reason: "OK",
        headers: &[
            ("Transfer-Encoding", "gzip, chunked"),
            ("Content-Length", "3"),
        ],
        framing: Framing::Chunked,
    },
    ResponseCase {
        raw: b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\ntransfer-encoding: CHUNKED\r\n\r\n",
        status: 200,
        reason: "OK",
        headers: &[
            ("Transfer-Encoding", "gzip"),
            ("transfer-encoding", "CHUNKED"),
        ],
        framing: Framing::Chunked,
    },
    ResponseCase {
        raw: b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked, gzip\r\n\r\n",
        status: 200,
        reason: "OK",
        headers: &[("Transfer-Encoding", "chunked, gzip")],
        framing: Framing::UntilEof,
    },
    ResponseCase {
        raw: b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 5\r\nContent-Length: 05\r\n\r\nerror",
        status: 502,
        reason: "Bad Gateway",
        headers: &[("content-length", "5"), ("Content-Length", "05")],
        framing: Framing::Length(5),
    },
    ResponseCase {
        raw: b"HTTP/1.1 200 \xc3\x84rger\r\nX-Name: h\xc3\xa9llo\xc2\xa0\r\nX-Bad: \xff\r\n\r\n",
        status: 200,
        reason: "",
        headers: &[("X-Name", "h\u{e9}llo"), ("X-Bad", "\u{fffd}")],
        framing: Framing::UntilEof,
    },
];

#[test]
fn response_table_matches_old_parser() {
    for case in RESPONSES {
        let context = String::from_utf8_lossy(case.raw);
        let (status, headers, _) = old::parse_response(case.raw).unwrap();
        assert_eq!(status, case.status, "{context:?}");
        assert_eq!(headers, owned(case.headers), "{context:?}");

        let head = res(case.raw).unwrap().unwrap();
        assert_eq!(head.status, case.status, "{context:?}");
        assert_eq!(head.reason, case.reason, "{context:?}");
        assert_eq!(head.headers, owned(case.headers), "{context:?}");
        assert_eq!(head.framing, case.framing, "{context:?}");
        assert_response_matches_old(case.raw, &head);
    }
}

#[test]
fn response_table_at_every_split() {
    let mut rng = Rng(4);
    for case in RESPONSES {
        let expected = res(case.raw).unwrap().unwrap();
        check_head_splits(case.raw, expected.head_len, &expected, res, &mut rng);
    }
}

fn random_response(rng: &mut Rng) -> Vec<u8> {
    let mut raw = if rng.chance(90) {
        b"HTTP/1.1 ".to_vec()
    } else {
        b"HTTP/1.0 ".to_vec()
    };
    raw.extend_from_slice(format!("{:03}", rng.below(1000)).as_bytes());
    if rng.chance(90) {
        raw.push(b' ');
        for _ in 0..rng.below(20) {
            raw.push(rng.pick(b"abcdefghij KLMNOP-!~\t"));
        }
    }
    raw.extend_from_slice(b"\r\n");
    let mut lines = Vec::new();
    let count = rng.below(20);
    random_header_lines(rng, count, &mut lines);
    if rng.chance(30) {
        let n = rng.below(10_000);
        let line = content_length_line(rng, n);
        let at = line_boundary(&lines, rng);
        lines.splice(at..at, line);
    }
    if rng.chance(20) {
        let at = line_boundary(&lines, rng);
        lines.splice(at..at, b"Transfer-Encoding: chunked\r\n".to_vec());
    }
    raw.extend_from_slice(&lines);
    raw.extend_from_slice(b"\r\n");
    for _ in 0..rng.below(30) {
        raw.push(rng.next() as u8);
    }
    raw
}

#[test]
fn random_responses_match_old_parser() {
    let mut rng = Rng(5);
    for i in 0..3000 {
        let raw = random_response(&mut rng);
        let head = match res(&raw) {
            Ok(Some(head)) => head,
            other => panic!("{other:?} for {:?}", String::from_utf8_lossy(&raw)),
        };
        assert_response_matches_old(&raw, &head);
        if i < 100 {
            check_head_splits(&raw, head.head_len, &head, res, &mut rng);
        }
    }
}

#[test]
fn accepted_response_mutants_match_old_parser() {
    let mut rng = Rng(6);
    let (mut accepted, mut refused) = (0, 0);
    for _ in 0..30_000 {
        let mut raw = random_response(&mut rng);
        mutate(&mut rng, &mut raw);
        match res(&raw) {
            Ok(Some(head)) if !raw.starts_with(b"\r\n") => {
                assert_response_matches_old(&raw, &head);
                accepted += 1;
            }
            Ok(_) => {}
            Err(_) => refused += 1,
        }
    }
    assert!(accepted > 3000, "only {accepted} mutants accepted");
    assert!(refused > 3000, "only {refused} mutants refused");
}

#[test]
fn malformed_responses_are_refused() {
    let malformed: &[&[u8]] = &[
        b"HTTP/1.1 20 OK\r\n\r\n",
        b"HTTP/1.1 2000 OK\r\n\r\n",
        b"HTTP/2 200 OK\r\n\r\n",
        b"HTTP/1.1  200 OK\r\n\r\n",
        b"HTTP/1.1 200 OK\nX: y\n\n",
        b"HTTP/1.1 200 OK\r\nX: y\n\r\n",
        b"HTTP/1.1 200 OK\r\nX : y\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nX: a\r\n folded\r\n\r\n",
        b"HTTP/1.1 200 O\x00K\r\n\r\n",
        b"<html><body>not http</body></html>\r\n\r\n",
    ];
    for raw in malformed {
        let err = res(raw).expect_err(&format!("{:?}", String::from_utf8_lossy(raw)));
        assert!(matches!(err, HeadError::Malformed(_)), "{err:?}");
        check_head_error(raw, err, res);
    }
    let bad_length: &[&[u8]] = &[
        b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length: nope\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length: +1\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length: 99999999999999999999999\r\n\r\n",
    ];
    for raw in bad_length {
        check_head_error(raw, HeadError::BadContentLength, res);
    }
}

#[test]
fn response_head_limits() {
    let limits = Limits {
        max_head: 100,
        max_headers: 3,
        ..Limits::default()
    };
    let parse = |buf: &[u8]| parse_response_head(buf, &limits);
    let mut long = b"HTTP/1.1 200 OK\r\nX: ".to_vec();
    long.extend(std::iter::repeat_n(b'v', 200));
    long.extend_from_slice(b"\r\n\r\n");
    check_head_error(&long, HeadError::HeadTooLarge, parse);
    check_head_error(
        b"HTTP/1.1 200 OK\r\nA: 1\r\nB: 2\r\nC: 3\r\nD: 4\r\n\r\n",
        HeadError::TooManyHeaders,
        parse,
    );
    assert!(
        parse(b"HTTP/1.1 200 OK\r\nA: 1\r\nB: 2\r\nC: 3\r\n\r\n")
            .unwrap()
            .is_some()
    );
    // Response bodies stream, so max_body does not apply.
    let limits = Limits {
        max_body: 1,
        ..Limits::default()
    };
    let head = parse_response_head(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n", &limits);
    assert_eq!(head.unwrap().unwrap().framing, Framing::Length(1000));
}

// ---------------------------------------------------------------------------
// Writers: golden bytes from the old server and recorder.
// ---------------------------------------------------------------------------

fn sse_response(body: &str) -> Response {
    Response {
        status: 200,
        reason: "OK".into(),
        headers: owned(&[
            ("Content-Type", "text/event-stream"),
            ("Cache-Control", "no-cache"),
            ("Transfer-Encoding", "chunked"),
            ("Connection", "close"),
        ]),
        body: Body::Chunked(vec![body.as_bytes().to_vec()]),
    }
}

#[test]
fn sse_response_bytes_match_old_server() {
    let body = "data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n\
                data: [DONE]\n\n";
    let expected = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/event-stream\r\n\
         Cache-Control: no-cache\r\n\
         Transfer-Encoding: chunked\r\n\
         Connection: close\r\n\
         \r\n\
         {:x}\r\n{body}\r\n0\r\n\r\n",
        body.len()
    );
    let written = write_response(&sse_response(body));
    assert_eq!(String::from_utf8_lossy(&written), expected);
    assert_eq!(written, old::write_sse_response(body));

    // A body long enough for a multi-digit hex size, with non-ASCII text.
    let long = "event: content_block_delta\ndata: {\"text\":\"\u{e9}t\u{e9}\"}\n\n".repeat(40);
    assert!(long.len() > 0x100);
    assert_eq!(
        write_response(&sse_response(&long)),
        old::write_sse_response(&long)
    );
}

/// The caveat: an empty part is framed as `0\r\n\r\n`, exactly as the old
/// server framed an empty SSE body.
#[test]
fn empty_sse_body_matches_old_server() {
    let written = write_response(&sse_response(""));
    assert_eq!(written, old::write_sse_response(""));
    assert!(written.ends_with(b"close\r\n\r\n0\r\n\r\n0\r\n\r\n"));
}

#[test]
fn http_error_bytes_match_old_server() {
    let body = r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#;
    let extra = owned(&[("retry-after", "3"), ("x-request-id", "req_01")]);
    let mut headers = owned(&[
        ("Content-Type", "application/json"),
        ("Content-Length", &body.len().to_string()),
        ("Connection", "close"),
    ]);
    headers.extend(extra.iter().cloned());
    let response = Response {
        status: 429,
        reason: "Too Many Requests".into(),
        headers,
        body: Body::Full(body.as_bytes().to_vec()),
    };
    let expected = format!(
        "HTTP/1.1 429 Too Many Requests\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         retry-after: 3\r\n\
         x-request-id: req_01\r\n\
         \r\n\
         {body}",
        body.len()
    );
    let written = write_response(&response);
    assert_eq!(String::from_utf8_lossy(&written), expected);
    assert_eq!(
        written,
        old::write_http_error(429, "Too Many Requests", "application/json", &extra, body)
    );

    // The unsupported-action 501, with the old fallback reason for an
    // unlisted status.
    let body = r#"{"error":{"message":"script action is not implemented by jig-server yet"}}"#;
    let response = Response {
        status: 501,
        reason: "Not Implemented".into(),
        headers: owned(&[
            ("Content-Type", "application/json"),
            ("Content-Length", &body.len().to_string()),
            ("Connection", "close"),
        ]),
        body: Body::Full(body.as_bytes().to_vec()),
    };
    assert_eq!(
        write_response(&response),
        old::write_http_error(501, "Not Implemented", "application/json", &[], body)
    );
    let response = Response {
        status: 418,
        reason: "Error".into(),
        headers: owned(&[
            ("Content-Type", "text/plain"),
            ("Content-Length", "0"),
            ("Connection", "close"),
        ]),
        body: Body::Full(Vec::new()),
    };
    assert_eq!(
        write_response(&response),
        old::write_http_error(418, "Error", "text/plain", &[], "")
    );
}

#[test]
fn not_found_and_preflight_bytes_match_old() {
    let not_found = Response {
        status: 404,
        reason: "Not Found".into(),
        headers: owned(&[("Content-Length", "0"), ("Connection", "close")]),
        body: Body::Empty,
    };
    assert_eq!(write_response(&not_found), old::write_not_found());
    assert_eq!(
        write_response(&not_found),
        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );

    let preflight = Response {
        status: 204,
        reason: "No Content".into(),
        headers: owned(&[("Content-Length", "0"), ("Connection", "close")]),
        body: Body::Empty,
    };
    assert_eq!(write_response(&preflight), old::write_preflight());
}

#[test]
fn chunked_parts_are_framed_in_order() {
    let response = Response {
        status: 200,
        reason: "OK".into(),
        headers: owned(&[("Transfer-Encoding", "chunked")]),
        body: Body::Chunked(vec![b"abc".to_vec(), vec![b'x'; 26], b"de".to_vec()]),
    };
    let mut expected =
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n1a\r\n".to_vec();
    expected.extend_from_slice(&[b'x'; 26]);
    expected.extend_from_slice(b"\r\n2\r\nde\r\n0\r\n\r\n");
    assert_eq!(write_response(&response), expected);

    let none = Response {
        body: Body::Chunked(Vec::new()),
        ..response
    };
    assert!(write_response(&none).ends_with(b"chunked\r\n\r\n0\r\n\r\n"));
}

#[test]
fn written_responses_parse_back() {
    let response = Response {
        status: 599,
        reason: "Custom Reason".into(),
        headers: owned(&[("X-A", "1"), ("content-length", "4")]),
        body: Body::Full(b"body".to_vec()),
    };
    let wire = write_response(&response);
    let head = res(&wire).unwrap().unwrap();
    assert_eq!(head.status, 599);
    assert_eq!(head.reason, "Custom Reason");
    assert_eq!(head.headers, response.headers);
    assert_eq!(head.framing, Framing::Length(4));
    assert_eq!(&wire[head.head_len..], b"body");
}

#[test]
fn request_head_matches_old_upstream_head() {
    // What the recorder core forwards for the old proxy test's request:
    // Host dropped and re-added for the upstream, Accept-Encoding forced to
    // identity, Connection: close appended.
    let client = owned(&[
        ("Host", "127.0.0.1:5050"),
        ("Accept-Encoding", "gzip, br"),
        ("Authorization", "Bearer sk-x"),
    ]);
    let forwarded = owned(&[
        ("Accept-Encoding", "identity"),
        ("Authorization", "Bearer sk-x"),
        ("Host", "api.openai.com"),
        ("Connection", "close"),
    ]);
    let written = write_request_head("POST", "/chat/completions?x=1", &forwarded);
    assert_eq!(
        String::from_utf8_lossy(&written),
        old::build_upstream_request_head(
            "POST",
            "/chat/completions?x=1",
            &client,
            "api.openai.com"
        )
    );
    assert_eq!(
        written,
        b"POST /chat/completions?x=1 HTTP/1.1\r\nAccept-Encoding: identity\r\n\
          Authorization: Bearer sk-x\r\nHost: api.openai.com\r\nConnection: close\r\n\r\n"
    );

    let head = req(&written).unwrap().unwrap();
    assert_eq!(head.method, "POST");
    assert_eq!(head.target, "/chat/completions?x=1");
    assert_eq!(head.headers, forwarded);
    assert_eq!(head.head_len, written.len());
    assert_eq!(
        write_request_head("GET", "/", &[]),
        b"GET / HTTP/1.1\r\n\r\n"
    );
}

// ---------------------------------------------------------------------------
// The chunked decoder.
// ---------------------------------------------------------------------------

/// The chunked body `write_response` produces for `parts`.
fn encode(parts: &[Vec<u8>]) -> Vec<u8> {
    let wire = write_response(&Response {
        status: 200,
        reason: "OK".into(),
        headers: owned(&[("Transfer-Encoding", "chunked")]),
        body: Body::Chunked(parts.to_vec()),
    });
    let head = res(&wire).unwrap().unwrap();
    assert_eq!(head.framing, Framing::Chunked);
    wire[head.head_len..].to_vec()
}

/// Feeds `input` in pieces ending at `cuts`, as a planner would. Returns the
/// decoded bytes and the input used once the body ends, `None` if it never
/// did.
fn decode_in_pieces(input: &[u8], cuts: &[usize]) -> Result<Option<(Vec<u8>, usize)>, ChunkError> {
    let mut decoder = ChunkedDecoder::new();
    let mut out = Vec::new();
    let mut used = 0;
    let mut start = 0;
    for end in cuts.iter().copied().chain([input.len()]) {
        let piece = &input[start..end];
        start = end;
        let progress = decoder.feed(piece, &mut out)?;
        used += progress.consumed;
        if progress.done {
            assert!(decoder.is_done());
            return Ok(Some((out, used)));
        }
        assert_eq!(
            progress.consumed,
            piece.len(),
            "stopped early without finishing"
        );
    }
    Ok(None)
}

/// Every two-piece split, byte at a time, and random splits all decode
/// `input` to `expected`, using exactly `body_len` bytes of it.
fn check_decode(input: &[u8], expected: &[u8], body_len: usize, rng: &mut Rng) {
    let mut splits: Vec<Vec<usize>> = (0..=input.len()).map(|cut| vec![cut]).collect();
    splits.push((1..input.len()).collect());
    splits.extend((0..20).map(|_| rng.cuts(input.len())));
    for cuts in splits {
        let decoded = decode_in_pieces(input, &cuts).unwrap_or_else(|err| {
            panic!(
                "{err} at cuts {cuts:?}: {:?}",
                String::from_utf8_lossy(input)
            )
        });
        let (out, used) = decoded.unwrap_or_else(|| panic!("unfinished at cuts {cuts:?}"));
        assert_eq!(out, expected, "cuts {cuts:?}");
        assert_eq!(used, body_len, "cuts {cuts:?}");
    }
}

/// Every split fails with `expected`, and the failure sticks.
fn check_decode_error(input: &[u8], expected: ChunkError) {
    let mut splits: Vec<Vec<usize>> = (0..=input.len()).map(|cut| vec![cut]).collect();
    splits.push((1..input.len()).collect());
    for cuts in splits {
        assert_eq!(
            decode_in_pieces(input, &cuts),
            Err(expected),
            "cuts {cuts:?}: {:?}",
            String::from_utf8_lossy(input)
        );
    }
    let mut decoder = ChunkedDecoder::new();
    let mut out = Vec::new();
    assert_eq!(decoder.feed(input, &mut out), Err(expected));
    assert_eq!(decoder.feed(b"0\r\n\r\n", &mut out), Err(expected));
    assert_eq!(decoder.feed(b"", &mut out), Err(expected));
}

#[test]
fn chunked_round_trips_at_every_split() {
    let mut rng = Rng(7);
    for _ in 0..60 {
        let parts: Vec<Vec<u8>> = (0..rng.below(6) + 1)
            .map(|_| {
                let len = if rng.chance(20) {
                    256 + rng.below(300)
                } else {
                    1 + rng.below(40)
                };
                (0..len).map(|_| rng.next() as u8).collect()
            })
            .collect();
        let body = encode(&parts);
        let expected = parts.concat();
        // Bytes after the body are left for the caller.
        let mut input = body.clone();
        input.extend_from_slice(b"HTTP/1.1 200 OK\r\n");
        check_decode(&input, &expected, body.len(), &mut rng);
    }
}

#[test]
fn chunked_extensions_trailers_and_spelling() {
    let cases: &[(&[u8], &[u8])] = &[
        (b"0\r\n\r\n", b""),
        (b"5\r\nhello\r\n0\r\n\r\n", b"hello"),
        (
            b"5;name=value\r\nhello\r\n6 ; a=\"q;x\" ; b\r\n world\r\n0;last\r\n\r\n",
            b"hello world",
        ),
        (b"5;\tx=\"\xc3\xa9 tab\"\r\nhello\r\n0\r\n\r\n", b"hello"),
        (
            b"A\r\n0123456789\r\na\r\nabcdefghij\r\n0\r\n\r\n",
            b"0123456789abcdefghij",
        ),
        (b"0005\r\nhello\r\n000\r\n\r\n", b"hello"),
        (b"00000000000000000001\r\nx\r\n0\r\n\r\n", b"x"),
        (b"3  \r\nabc\r\n0\t\r\n\r\n", b"abc"),
        (
            b"3\r\nabc\r\n0\r\nX-Checksum: 123\r\nEmpty:\r\nY: a\tb \xc3\xa9\r\n\r\n",
            b"abc",
        ),
        // Chunk data may hold anything, CRLFs included.
        (b"4\r\n\r\n\r\n\r\n0\r\n\r\n", b"\r\n\r\n"),
    ];
    let mut rng = Rng(8);
    for (body, expected) in cases {
        let mut input = body.to_vec();
        input.extend_from_slice(b"next");
        check_decode(&input, expected, body.len(), &mut rng);
    }
}

#[test]
fn malformed_chunked_bodies_are_refused() {
    let cases: &[(&[u8], ChunkError)] = &[
        (b"G\r\n", ChunkError::BadSizeLine),
        (b"\r\n", ChunkError::BadSizeLine),
        (b"\n", ChunkError::BadSizeLine),
        (b" 5\r\n", ChunkError::BadSizeLine),
        (b"-5\r\n", ChunkError::BadSizeLine),
        (b"+5\r\n", ChunkError::BadSizeLine),
        (b"0x5\r\n", ChunkError::BadSizeLine),
        (b"5 x\r\n", ChunkError::BadSizeLine),
        (b"5;a\x00\r\n", ChunkError::BadSizeLine),
        (b"5;a\x7f\r\n", ChunkError::BadSizeLine),
        (b"5\n", ChunkError::BadLineEnding),
        (b"5 \n", ChunkError::BadLineEnding),
        (b"5;a\nb", ChunkError::BadLineEnding),
        (b"5\rX", ChunkError::BadLineEnding),
        (b"5\r\nhelloX", ChunkError::BadLineEnding),
        (b"5\r\nhello\rX", ChunkError::BadLineEnding),
        (b"5\r\nhello\n", ChunkError::BadLineEnding),
        (b"5\r\nhello\r\n\r\n", ChunkError::BadSizeLine),
        (b"10000000000000000\r\n", ChunkError::SizeOverflow),
        (b"FFFFFFFFFFFFFFFFF\r\n", ChunkError::SizeOverflow),
        (b"0\r\nno-colon\r\n\r\n", ChunkError::BadTrailer),
        (b"0\r\n: v\r\n\r\n", ChunkError::BadTrailer),
        (b"0\r\n folded: v\r\n\r\n", ChunkError::BadTrailer),
        (b"0\r\nBad Name: v\r\n\r\n", ChunkError::BadTrailer),
        (b"0\r\nX: a\x01\r\n\r\n", ChunkError::BadTrailer),
        (b"0\r\nX: v\n", ChunkError::BadLineEnding),
        (b"0\r\nX: v\rY", ChunkError::BadLineEnding),
        (b"0\r\n\rX", ChunkError::BadLineEnding),
        (b"0\r\n\n", ChunkError::BadLineEnding),
        (b"0\r\nX\n", ChunkError::BadLineEnding),
    ];
    for (input, expected) in cases {
        check_decode_error(input, *expected);
    }
}

#[test]
fn chunk_size_line_is_bounded() {
    let mut rng = Rng(9);
    let mut at_limit = b"5;".to_vec();
    at_limit.extend(std::iter::repeat_n(b'a', MAX_CHUNK_LINE - 2));
    at_limit.extend_from_slice(b"\r\nhello\r\n0\r\n\r\n");
    check_decode(&at_limit, b"hello", at_limit.len(), &mut rng);

    let mut over = b"5;".to_vec();
    over.extend(std::iter::repeat_n(b'a', MAX_CHUNK_LINE - 1));
    over.extend_from_slice(b"\r\nhello\r\n0\r\n\r\n");
    check_decode_error(&over, ChunkError::LineTooLong);

    // Leading zeros count toward the line too.
    let mut zeros = vec![b'0'; MAX_CHUNK_LINE];
    zeros.extend_from_slice(b"1\r\n");
    check_decode_error(&zeros, ChunkError::LineTooLong);
}

/// A last chunk whose trailer section (everything after `0\r\n`) is `len`
/// bytes.
fn last_chunk_with_trailers(len: usize) -> Vec<u8> {
    let mut body = b"0\r\n".to_vec();
    let mut left = len - 2;
    while left > 0 {
        let line = left.min(1000);
        assert!(line >= 5, "trailer line too short");
        body.extend_from_slice(b"X: ");
        body.extend(std::iter::repeat_n(b'v', line - 5));
        body.extend_from_slice(b"\r\n");
        left -= line;
        if (1..5).contains(&left) {
            // Rebalance so the last line is long enough.
            let pad = 5 - left;
            body.truncate(body.len() - 2 - pad);
            body.extend_from_slice(b"\r\n");
            left += pad;
        }
    }
    body.extend_from_slice(b"\r\n");
    body
}

#[test]
fn trailer_section_is_bounded() {
    let mut rng = Rng(10);
    let at_limit = last_chunk_with_trailers(MAX_TRAILER_SECTION);
    assert_eq!(at_limit.len(), 3 + MAX_TRAILER_SECTION);
    let mut decoder = ChunkedDecoder::new();
    let mut out = Vec::new();
    assert_eq!(
        decoder.feed(&at_limit, &mut out),
        Ok(Progress {
            consumed: at_limit.len(),
            done: true
        })
    );
    for _ in 0..5 {
        let cuts = rng.cuts(at_limit.len());
        let decoded = decode_in_pieces(&at_limit, &cuts).unwrap().unwrap();
        assert_eq!(decoded, (Vec::new(), at_limit.len()));
    }

    let over = last_chunk_with_trailers(MAX_TRAILER_SECTION + 1);
    assert_eq!(over.len(), 4 + MAX_TRAILER_SECTION);
    let mut decoder = ChunkedDecoder::new();
    assert_eq!(
        decoder.feed(&over, &mut out),
        Err(ChunkError::TrailersTooLarge)
    );
}

#[test]
fn largest_chunk_size_is_accepted() {
    let mut decoder = ChunkedDecoder::new();
    let mut out = Vec::new();
    let progress = decoder.feed(b"FFFFFFFFFFFFFFFF\r\nab", &mut out).unwrap();
    assert_eq!(
        progress,
        Progress {
            consumed: 20,
            done: false
        }
    );
    assert_eq!(out, b"ab");
}

/// The write-side caveat seen from the read side: an empty part ends the
/// body, and what the writer put after it is left unread.
#[test]
fn empty_part_ends_the_body() {
    let body = encode(&[Vec::new(), b"abc".to_vec()]);
    assert_eq!(body, b"0\r\n\r\n3\r\nabc\r\n0\r\n\r\n");
    let mut decoder = ChunkedDecoder::new();
    let mut out = Vec::new();
    assert_eq!(
        decoder.feed(&body, &mut out),
        Ok(Progress {
            consumed: 5,
            done: true
        })
    );
    assert!(out.is_empty());
}

#[test]
fn decoder_after_the_end_and_on_empty_input() {
    let mut decoder = ChunkedDecoder::default();
    let mut out = Vec::new();
    assert_eq!(
        decoder.feed(b"", &mut out),
        Ok(Progress {
            consumed: 0,
            done: false
        })
    );
    assert!(!decoder.is_done());
    decoder.feed(b"1\r\nx\r\n0\r\n\r\n", &mut out).unwrap();
    assert!(decoder.is_done());
    assert_eq!(
        decoder.feed(b"more", &mut out),
        Ok(Progress {
            consumed: 0,
            done: true
        })
    );
    assert_eq!(out, b"x");
}

#[test]
fn errors_render_for_the_planners() {
    assert_eq!(
        HeadError::Malformed("invalid header name").to_string(),
        "malformed HTTP head: invalid header name"
    );
    assert_eq!(ChunkError::SizeOverflow.to_string(), "chunk size overflows");
}

#[test]
fn reparse_parses_only_at_a_newline_or_the_limit() {
    let limits = Limits::default();
    let mut calls = 0;
    let mut scanned = 0;
    let mut feed = |buf: &[u8], scanned: &mut usize| {
        reparse(buf, scanned, &limits, |buf, limits| {
            calls += 1;
            parse_request_head(buf, limits)
        })
    };
    assert_eq!(feed(b"GET / HT", &mut scanned), Ok(None));
    assert_eq!(scanned, 8);
    assert_eq!(feed(b"GET / HTTP/1.1\r\nHo", &mut scanned), Ok(None));
    assert_eq!(feed(b"GET / HTTP/1.1\r\nHost: h", &mut scanned), Ok(None));
    let whole = b"GET / HTTP/1.1\r\nHost: h\r\n\r\n";
    assert!(matches!(feed(whole, &mut scanned), Ok(Some(_))));
    assert_eq!(calls, 2, "one parse per newline that arrived");

    // A head that reaches the limit without a newline is parsed, and fails.
    let small = Limits {
        max_head: 8,
        ..Limits::default()
    };
    let mut scanned = 0;
    let got = reparse(b"GET /aaaa", &mut scanned, &small, parse_request_head);
    assert_eq!(got, Err(HeadError::HeadTooLarge));
}
