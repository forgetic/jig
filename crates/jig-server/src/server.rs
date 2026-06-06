//! The async HTTP/1.1 + chunked-SSE server, hand-rolled on a tokio socket.
//!
//! Kept deliberately tiny: this is a single-threaded, low-traffic test double,
//! not a server under load (see bootstrap.md "Runtime & HTTP layer"). We read
//! just enough of each request to route on the path, then stream the rendered
//! SSE frames as an HTTP/1.1 chunked body.

use std::io;
use std::sync::{Arc, Mutex};

use jig_core::request::{parse_anthropic, parse_openai};
use jig_core::{
    Dialect, RecordedRequest, RequestView, Script, render::frames_to_body, render_anthropic,
    render_openai,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

/// Shared, append-only log of every request the server handled, in arrival
/// order. Held behind `Arc<Mutex<…>>` so it is reachable from both the runtime
/// thread (which appends) and the caller's thread (which reads via
/// `FakeLlm::requests()`).
pub type RequestLog = Arc<Mutex<Vec<RecordedRequest>>>;

/// Run the accept loop until `shutdown` fires, then return.
///
/// `listener` is already bound (the caller binds before spawning so `base_url`
/// is valid immediately). Each accepted connection is handled inline — the
/// single-threaded runtime keeps ordering deterministic, which is also what lets
/// `Sequence` advance and the request log append in a stable order.
pub async fn serve(
    listener: TcpListener,
    script: Arc<Script>,
    log: RequestLog,
    mut shutdown: oneshot::Receiver<()>,
) {
    loop {
        tokio::select! {
            // Drop signalled shutdown: stop accepting and unwind.
            _ = &mut shutdown => return,
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _peer)) => {
                        let script = Arc::clone(&script);
                        let log = Arc::clone(&log);
                        // Handle inline; connections are short-lived SSE streams.
                        if let Err(err) = handle_connection(stream, &script, &log).await {
                            // A client hang-up mid-stream is normal for a test
                            // double; never let it take the server down.
                            let _ = err;
                        }
                    }
                    Err(_) => return,
                }
            }
        }
    }
}

/// Read one request, record it, route it, and stream the response.
async fn handle_connection(
    mut stream: TcpStream,
    script: &Script,
    log: &RequestLog,
) -> io::Result<()> {
    let request = read_request(&mut stream).await?;

    // Project the body for the matched dialect (if any) so it is available both
    // to the script and to the recorded request.
    let view = dialect_for_path(&request.path).map(|dialect| match dialect {
        Dialect::OpenAi => parse_openai(&request.body),
        Dialect::Anthropic => parse_anthropic(&request.body),
        // M4 adds the Codex projection; until then that route does not exist (it
        // 404s), so this arm is unreachable in M3.
        Dialect::Codex => parse_openai(&request.body),
    });

    // Record before responding so a captured request reflects exactly what the
    // client sent, regardless of how the response goes.
    record_request(log, &request, view.clone());

    match request.path.as_str() {
        "/chat/completions" => {
            // Every dialect route has a projected view; default to an empty
            // OpenAI view if projection somehow yielded nothing.
            let view = view.unwrap_or_else(empty_openai_view);
            let reply = script.next_reply(&view);
            let body = frames_to_body(&render_openai(&reply));
            write_sse_response(&mut stream, &body).await
        }
        "/v1/messages" => {
            // Anthropic messages dialect. Same script seam as OpenAI — only the
            // renderer differs.
            let view = view.unwrap_or_else(empty_anthropic_view);
            let reply = script.next_reply(&view);
            let body = frames_to_body(&render_anthropic(&reply));
            write_sse_response(&mut stream, &body).await
        }
        _ => write_not_found(&mut stream).await,
    }
}

/// Map a request path to the wire dialect it serves, or `None` for unknown
/// paths (which `404`). The route table is the single source of dialect truth
/// (see bootstrap.md "Why this shape").
fn dialect_for_path(path: &str) -> Option<Dialect> {
    match path {
        "/chat/completions" => Some(Dialect::OpenAi),
        "/v1/messages" => Some(Dialect::Anthropic),
        _ => None,
    }
}

/// An empty OpenAI view — the fallback when a request body fails to project.
fn empty_openai_view() -> RequestView {
    RequestView::new(Dialect::OpenAi, None, Vec::new(), 0)
}

/// An empty Anthropic view — the fallback when a request body fails to project.
fn empty_anthropic_view() -> RequestView {
    RequestView::new(Dialect::Anthropic, None, Vec::new(), 0)
}

/// Append a [`RecordedRequest`] to the shared log.
fn record_request(log: &RequestLog, request: &Request, view: Option<RequestView>) {
    let recorded = RecordedRequest {
        path: request.path.clone(),
        method: request.method.clone(),
        body: request.body.clone(),
        view,
    };
    // A poisoned lock should not crash the runtime thread; recover the guard.
    let mut guard = log.lock().unwrap_or_else(|p| p.into_inner());
    guard.push(recorded);
}

/// A parsed request — path, method, and the (fully read) body.
struct Request {
    path: String,
    method: String,
    body: Vec<u8>,
}

/// Read the request line + headers, then read the full body declared by
/// `Content-Length`. The body is captured (not just drained) so it can be parsed
/// into a `RequestView` and recorded for assertions; reading it fully also keeps
/// the socket clean so clients that wait for us to read don't stall.
async fn read_request(stream: &mut TcpStream) -> io::Result<Request> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];

    // Read until we have the full header block (terminated by CRLFCRLF).
    let header_end = loop {
        if let Some(pos) = find_header_end(&buf) {
            break pos;
        }
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before headers completed",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let header_text = String::from_utf8_lossy(&buf[..header_end]);
    let mut lines = header_text.split("\r\n");

    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let raw_target = parts.next().unwrap_or("/");
    // Strip any query string for routing purposes.
    let path = raw_target.split('?').next().unwrap_or("/").to_string();

    let mut content_length = 0usize;
    for line in lines {
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }

    // The body bytes already sitting in `buf` after the header terminator.
    let body_start = header_end + 4;
    let mut body = buf[body_start..].to_vec();

    // Read the remainder of the declared body.
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

    Ok(Request { path, method, body })
}

/// Find the byte index of the end of the header block (the `\r\n\r\n` start).
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Write a `200` SSE response with the body as a single HTTP/1.1 chunk.
///
/// Chunked transfer-encoding is what real providers use; emitting the whole
/// body as one chunk is sufficient for the SDK parser and keeps the writer
/// trivial. `Connection: close` lets the client treat EOF as end-of-stream.
async fn write_sse_response(stream: &mut TcpStream, body: &str) -> io::Result<()> {
    let headers = "HTTP/1.1 200 OK\r\n\
         Content-Type: text/event-stream\r\n\
         Cache-Control: no-cache\r\n\
         Transfer-Encoding: chunked\r\n\
         Connection: close\r\n\
         \r\n";
    stream.write_all(headers.as_bytes()).await?;

    // One chunk: "<hex len>\r\n<body>\r\n", then the zero-length terminator.
    let chunk_header = format!("{:x}\r\n", body.len());
    stream.write_all(chunk_header.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.write_all(b"\r\n0\r\n\r\n").await?;
    stream.flush().await?;
    Ok(())
}

/// Write a bare `404` for unknown paths.
async fn write_not_found(stream: &mut TcpStream) -> io::Result<()> {
    let response = "HTTP/1.1 404 Not Found\r\n\
         Content-Length: 0\r\n\
         Connection: close\r\n\
         \r\n";
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}
