//! The exchange as the recorder sees it, and the one rewrite it makes on the
//! way upstream.
//!
//! These are the pure pieces of the proxy: the request read from the client,
//! the response captured from the upstream, and the request head forwarded
//! upstream. The relaying itself is [`crate::relay`]'s. Keeping them apart
//! means the capture format (which fixtures depend on) doesn't move when the
//! I/O around it does.

use crate::redact::Header;
use crate::route::Route;

/// A request read from the downstream client: method, raw target (path +
/// optional query), headers, and the fully-read body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientRequest {
    pub method: String,
    /// The request target as sent (may include a query string).
    pub target: String,
    pub headers: Vec<Header>,
    pub body: Vec<u8>,
}

impl ClientRequest {
    /// The path with any query string stripped — what routing keys on.
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or("/")
    }
}

/// A response captured from the upstream: status code, headers, and the raw
/// body bytes (the SSE stream) exactly as received, chunk framing included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamResponse {
    pub status: u16,
    pub headers: Vec<Header>,
    pub body: Vec<u8>,
}

/// Render the request head to send upstream: the original request line and
/// headers, with `Host` pointed at the upstream and `Accept-Encoding: identity`
/// forced so the captured SSE body is uncompressed plaintext. The body follows
/// separately.
pub(crate) fn build_upstream_request_head(request: &ClientRequest, route: &Route) -> String {
    let mut head = format!("{} {} HTTP/1.1\r\n", request.method, request.target);
    let mut saw_accept_encoding = false;
    for h in &request.headers {
        if h.name.eq_ignore_ascii_case("host") {
            // Rewritten below to the real upstream.
            continue;
        }
        if h.name.eq_ignore_ascii_case("accept-encoding") {
            head.push_str("Accept-Encoding: identity\r\n");
            saw_accept_encoding = true;
            continue;
        }
        head.push_str(&format!("{}: {}\r\n", h.name, h.value));
    }
    head.push_str(&format!("Host: {}\r\n", route.upstream_host));
    if !saw_accept_encoding {
        head.push_str("Accept-Encoding: identity\r\n");
    }
    head.push_str("Connection: close\r\n\r\n");
    head
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_request_strips_query_for_routing() {
        let req = ClientRequest {
            method: "POST".to_string(),
            target: "/chat/completions?stream=true".to_string(),
            headers: vec![],
            body: vec![],
        };
        assert_eq!(req.path(), "/chat/completions");
    }

    #[test]
    fn upstream_head_rewrites_host_and_forces_identity_encoding() {
        let req = ClientRequest {
            method: "POST".to_string(),
            target: "/chat/completions".to_string(),
            headers: vec![
                Header::new("Host", "127.0.0.1:5050"),
                Header::new("Accept-Encoding", "gzip, br"),
                Header::new("Authorization", "Bearer sk-x"),
            ],
            body: b"{}".to_vec(),
        };
        let route = Route::resolve("/chat/completions").unwrap();
        let head = build_upstream_request_head(&req, &route);

        assert!(head.starts_with("POST /chat/completions HTTP/1.1\r\n"));
        assert!(head.contains("Host: api.openai.com\r\n"));
        // Original loopback Host is gone.
        assert!(!head.contains("127.0.0.1:5050"));
        // Compression is neutralized and not duplicated.
        assert!(head.contains("Accept-Encoding: identity\r\n"));
        assert_eq!(head.matches("Accept-Encoding:").count(), 1);
        // Auth header is forwarded as-is to the real upstream (redaction happens
        // only on the *captured* copy, never on the wire).
        assert!(head.contains("Authorization: Bearer sk-x\r\n"));
        assert!(head.ends_with("\r\n\r\n"));
    }

    #[test]
    fn upstream_head_adds_identity_when_client_sent_none() {
        let req = ClientRequest {
            method: "POST".to_string(),
            target: "/v1/messages".to_string(),
            headers: vec![Header::new("Host", "localhost")],
            body: vec![],
        };
        let route = Route::resolve("/v1/messages").unwrap();
        let head = build_upstream_request_head(&req, &route);
        assert!(head.contains("Accept-Encoding: identity\r\n"));
        assert!(head.contains("Host: api.anthropic.com\r\n"));
    }
}
