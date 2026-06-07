//! The passthrough proxy: the one async, network-touching part of the recorder.
//!
//! It binds `127.0.0.1:0`, accepts a single client connection, routes the
//! request by path to a dialect + upstream ([`crate::route::Route`]), opens an
//! **HTTPS** connection to that upstream, forwards the request verbatim, and
//! streams the response back to the client **unbuffered** — every byte read from
//! the upstream is written to the client *and* appended to the capture buffer
//! before the next read, so SSE timing and framing are preserved (issue #18:
//! "forward bytes as they arrive; don't re-chunk or buffer the SSE body").
//!
//! This module is exercised **manually** against a real backend (recording is
//! manual, per the issue); the default `cargo test` suite stays network-free.
//! The pure pieces it leans on — routing, redaction, the fixture model — are
//! unit-tested in their own modules.

use std::io;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

use crate::redact::Header;
use crate::route::Route;

/// A request read from the downstream client: method, raw target (path +
/// optional query), headers, and the fully-read body.
#[derive(Debug, Clone)]
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
/// body bytes (the SSE stream) exactly as received.
#[derive(Debug, Clone)]
pub struct UpstreamResponse {
    pub status: u16,
    pub headers: Vec<Header>,
    pub body: Vec<u8>,
}

/// Bind the proxy listener on an ephemeral loopback port.
///
/// Bound separately from [`proxy_once`] so a caller can read the local address
/// (to point a client at it) before any connection arrives — the same shape as
/// `FakeLlm::start`.
pub async fn bind() -> io::Result<TcpListener> {
    TcpListener::bind(("127.0.0.1", 0)).await
}

/// Accept client connections until one carries a routable request, forward that
/// one to its upstream over HTTPS, stream the response back unbuffered, and
/// return the captured request/response for the fixture writer.
///
/// Real official clients open a **connectivity preflight** before the request
/// that matters: Claude Code, for instance, sends a `HEAD /` probe on its own
/// connection before the first `POST /v1/messages`. Those probes do not resolve
/// to a dialect route, so capturing the *first* connection blindly would grab the
/// probe (and reject it as "no route for path /") instead of the real exchange.
/// We therefore loop: a connection whose request does not resolve to a route is
/// answered with a minimal `204` so the client proceeds, and we move on to the
/// next connection until a routable request arrives — that one is the capture.
///
/// `upstream_host_override` lets the caller point OpenAI-dialect traffic at an
/// OpenAI-compatible backend (DeepSeek, a gateway). `None` uses the dialect
/// default.
pub async fn proxy_once(
    listener: &TcpListener,
    upstream_host_override: Option<&str>,
) -> io::Result<(ClientRequest, UpstreamResponse, Route)> {
    loop {
        let (client, _peer) = listener.accept().await?;
        if let Some(triple) = handle_connection(client, upstream_host_override).await? {
            return Ok(triple);
        }
        // A non-routable preflight was answered; wait for the next connection.
    }
}

/// Handle one already-accepted client connection: read its request, and either
/// forward+capture a routable request (returning the captured triple) or answer
/// a non-routable connectivity preflight with `204` and return `None`.
///
/// Split out from [`proxy_once`] so a caller can accept connections
/// **concurrently** and run one of these per connection. Real official clients
/// pre-open a pool of connections and pick one for the request that matters; a
/// strictly serial accept loop can block on an idle pooled socket and never
/// reach the one carrying the `POST`. Driving this per-connection on its own
/// task sidesteps that — each idle socket simply parks its own task.
pub async fn handle_connection(
    mut client: TcpStream,
    upstream_host_override: Option<&str>,
) -> io::Result<Option<(ClientRequest, UpstreamResponse, Route)>> {
    let request = read_client_request(&mut client).await?;

    let route = Route::resolve(request.path()).map(|r| match upstream_host_override {
        Some(host) => r.with_upstream_host(host),
        None => r,
    });

    let Some(route) = route else {
        // A non-routable connectivity preflight (e.g. Claude Code's `HEAD /`
        // probe). Acknowledge it so the client proceeds to its real request.
        let _ = answer_preflight(&mut client).await;
        return Ok(None);
    };

    let response = forward(&mut client, &request, &route).await?;
    Ok(Some((request, response, route)))
}

/// Send a minimal, bodyless `204 No Content` to a preflight connection so the
/// client treats the base URL as reachable and goes on to its real request.
/// Best-effort: a failure here just means we drop the probe connection.
async fn answer_preflight(client: &mut TcpStream) -> io::Result<()> {
    client
        .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .await?;
    client.flush().await
}

/// Read the request line, headers, and full (Content-Length) body from the
/// client. Mirrors `jig_server`'s reader but keeps every header (the recorder
/// needs them) instead of only Content-Length.
async fn read_client_request(stream: &mut TcpStream) -> io::Result<ClientRequest> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];

    let header_end = loop {
        if let Some(pos) = find_header_end(&buf) {
            break pos;
        }
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "client closed before headers completed",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
    };

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
            headers.push(Header::new(name, value));
        }
    }

    let body_start = header_end + 4;
    let mut body = buf[body_start..].to_vec();
    let mut remaining = content_length.saturating_sub(body.len());
    while remaining > 0 {
        let want = remaining.min(chunk.len());
        let n = stream.read(&mut chunk[..want]).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
        remaining -= n;
    }

    Ok(ClientRequest {
        method,
        target,
        headers,
        body,
    })
}

/// Open a TLS connection to the route's upstream, send the request verbatim
/// (with the `Host` header rewritten to the upstream and `Accept-Encoding`
/// neutralized so the captured SSE is plaintext), then pump the response back to
/// the client unbuffered while capturing it.
async fn forward(
    client: &mut TcpStream,
    request: &ClientRequest,
    route: &Route,
) -> io::Result<UpstreamResponse> {
    let connector = tls_connector();
    let tcp = TcpStream::connect((route.upstream_host.as_str(), route.upstream_port)).await?;
    let server_name = ServerName::try_from(route.upstream_host.clone())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let mut upstream = connector.connect(server_name, tcp).await?;

    let head = build_upstream_request_head(request, route);
    upstream.write_all(head.as_bytes()).await?;
    upstream.write_all(&request.body).await?;
    upstream.flush().await?;

    // Read the upstream's response head (status + headers), forwarding those
    // bytes to the client as we go.
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = find_header_end(&buf) {
            break pos;
        }
        let n = upstream.read(&mut chunk).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "upstream closed before response headers completed",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let (status, headers) = parse_response_head(&buf[..header_end]);

    // Forward the response head verbatim to the client.
    client.write_all(&buf[..header_end + 4]).await?;
    client.flush().await?;

    // Any body bytes already read past the header terminator.
    let mut body = buf[header_end + 4..].to_vec();
    if !body.is_empty() {
        client.write_all(&body).await?;
        client.flush().await?;
    }

    // Pump the rest of the body: read → write to client → append to capture,
    // flushing each read so SSE frames reach the client as they arrive. We do
    // not parse or de-chunk; the bytes are forwarded and captured verbatim.
    loop {
        let n = upstream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        client.write_all(&chunk[..n]).await?;
        client.flush().await?;
        body.extend_from_slice(&chunk[..n]);
    }

    Ok(UpstreamResponse {
        status,
        headers,
        body,
    })
}

/// Build a rustls-based [`TlsConnector`] trusting the Mozilla webpki root set.
fn tls_connector() -> TlsConnector {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    TlsConnector::from(Arc::new(config))
}

/// Render the request head to send upstream: the original request line and
/// headers, with `Host` pointed at the upstream and `Accept-Encoding: identity`
/// forced so the captured SSE body is uncompressed plaintext. The body follows
/// separately.
fn build_upstream_request_head(request: &ClientRequest, route: &Route) -> String {
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

/// Parse a response head (`status-line CRLF headers`) into a status code and
/// header list.
fn parse_response_head(head: &[u8]) -> (u16, Vec<Header>) {
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
            headers.push(Header::new(name.trim(), value.trim()));
        }
    }
    (status, headers)
}

/// Find the byte index of the start of the `\r\n\r\n` header terminator.
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
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

    #[test]
    fn parse_response_head_extracts_status_and_headers() {
        let head = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nSet-Cookie: a=b";
        let (status, headers) = parse_response_head(head);
        assert_eq!(status, 200);
        assert!(
            headers
                .iter()
                .any(|h| h.name == "Content-Type" && h.value == "text/event-stream")
        );
        assert!(headers.iter().any(|h| h.name == "Set-Cookie"));
    }

    #[test]
    fn find_header_end_locates_terminator() {
        assert_eq!(find_header_end(b"abc\r\n\r\nbody"), Some(3));
        assert_eq!(find_header_end(b"no terminator"), None);
    }
}
