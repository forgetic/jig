//! HTTP/1.1: pure codec functions and the server and client planners.
//!
//! The message types below are shared by the codec, the planners and the
//! cores built on them (e.g. jig's provider core). They hold owned data only,
//! so they can cross step boundaries (house rule 2).

pub mod client;
pub mod codec;
pub mod server;

/// One HTTP exchange on the server side (one request per connection).
/// Allocated by the server planner; never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReqId(pub u64);

/// One upstream exchange on the client side (one request per connection).
/// Allocated by the core that starts the fetch; never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FetchId(pub u64);

/// A parsed request: head fields as received (header names keep their case,
/// values are trimmed of surrounding whitespace) and the complete body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    /// The request target as sent, query string included.
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    /// The target with any query string stripped: what routing keys on.
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or("/")
    }

    /// The first header with this name, compared case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// A response the planner writes verbatim: the status line from `status` and
/// `reason`, then `headers` in order, exactly as given (the core is
/// responsible for consistent framing headers), then the body framed as
/// `body` says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

/// How the response body goes on the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    /// No body bytes.
    Empty,
    /// Raw bytes (pair with a `Content-Length` header).
    Full(Vec<u8>),
    /// Each part becomes one chunk (`{len:x}\r\n{part}\r\n`), followed by the
    /// terminating `0\r\n\r\n` (pair with `Transfer-Encoding: chunked`).
    Chunked(Vec<Vec<u8>>),
}
