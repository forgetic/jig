//! The async HTTP/1.1 + chunked-SSE server, hand-rolled on a skein socket.
//!
//! Kept deliberately tiny: this is a single-threaded, low-traffic test double,
//! not a server under load (see bootstrap.md "Runtime & HTTP layer"). We read
//! just enough of each request to route on the path, then stream the rendered
//! SSE frames as an HTTP/1.1 chunked body.

use std::io;
use std::pin::pin;
use std::sync::{Arc, Mutex};

use jig_core::request::{parse_anthropic, parse_codex, parse_openai};
use jig_core::{
    Dialect, HttpError, RecordedRequest, Reply, RequestView, Script, ScriptAction,
    render::{SseFrame, frames_to_body},
    render_anthropic, render_codex, render_openai,
};
use jig_runtime::read_some;
use skein::combinator::{Either, Select};
use skein::cx::Cx;
use skein::io::AsyncWriteExt;
use skein::net::{TcpListener, TcpStream};
use skein::sync::Notify;

/// Shared, append-only log of every request the server handled, in arrival
/// order. Held behind `Arc<Mutex<…>>` so it is reachable from both the runtime
/// thread (which appends) and the caller's thread (which reads via
/// `FakeLlm::requests()`).
pub type RequestLog = Arc<Mutex<Vec<RecordedRequest>>>;

/// Run the accept loop until `shutdown` is notified, then return.
///
/// `listener` is already bound (the caller binds before spawning so `base_url`
/// is valid immediately). Each accepted connection is handled inline — the
/// single-threaded runtime keeps ordering deterministic, which is also what lets
/// `Sequence` advance and the request log append in a stable order.
pub async fn serve(
    cx: &Cx,
    listener: TcpListener,
    script: Arc<Script>,
    log: RequestLog,
    shutdown: Arc<Notify>,
) {
    loop {
        // Race shutdown against the next accept. Both futures are dropped at
        // the end of the iteration; `Notified` is cancel-safe and `notify_one`
        // stores an un-consumed notification, so a signal can never be lost
        // between iterations. Dropping the loser holds no obligations here.
        let notified = pin!(shutdown.notified());
        let accepted = pin!(listener.accept());
        match Select::new(notified, accepted).await {
            // Drop signalled shutdown: stop accepting and unwind.
            Either::Left(()) => return,
            Either::Right(Ok((stream, _peer))) => {
                let script = Arc::clone(&script);
                let log = Arc::clone(&log);
                // Handle inline; connections are short-lived SSE streams.
                if let Err(err) = handle_connection(cx, stream, &script, &log).await {
                    // A client hang-up mid-stream is normal for a test
                    // double; never let it take the server down.
                    let _ = err;
                }
            }
            Either::Right(Err(_)) => return,
        }
    }
}

/// Read one request, record it, route it, and stream the response.
async fn handle_connection(
    cx: &Cx,
    mut stream: TcpStream,
    script: &Script,
    log: &RequestLog,
) -> io::Result<()> {
    let request = read_request(cx, &mut stream).await?;

    // Project the body for the matched dialect (if any) so it is available both
    // to the script and to the recorded request.
    let view = dialect_for_path(&request.path).map(|dialect| match dialect {
        Dialect::OpenAi => parse_openai(&request.body),
        Dialect::Anthropic => parse_anthropic(&request.body),
        Dialect::Codex => parse_codex(&request.body),
    });

    // Record before responding so a captured request reflects exactly what the
    // client sent, regardless of how the response goes.
    record_request(log, &request, view.clone());

    match request.path.as_str() {
        "/chat/completions" => {
            // Every dialect route has a projected view; default to an empty
            // OpenAI view if projection somehow yielded nothing.
            let view = view.unwrap_or_else(empty_openai_view);
            let action = script.next_action(&view);
            write_action_response(cx, &mut stream, Dialect::OpenAi, action).await
        }
        "/v1/messages" => {
            // Anthropic messages dialect. Same script seam as OpenAI — only the
            // renderer differs.
            let view = view.unwrap_or_else(empty_anthropic_view);
            let action = script.next_action(&view);
            write_action_response(cx, &mut stream, Dialect::Anthropic, action).await
        }
        "/backend-api/codex/responses" => {
            // OpenAI Codex responses dialect. Same script seam as the others —
            // only the projection and renderer differ.
            let view = view.unwrap_or_else(empty_codex_view);
            let action = script.next_action(&view);
            write_action_response(cx, &mut stream, Dialect::Codex, action).await
        }
        _ => write_not_found(cx, &mut stream).await,
    }
}

async fn write_action_response(
    cx: &Cx,
    stream: &mut TcpStream,
    dialect: Dialect,
    action: ScriptAction,
) -> io::Result<()> {
    match action {
        ScriptAction::Reply(reply) => {
            let body = frames_to_body(&render_reply(dialect, &reply));
            write_sse_response(cx, stream, &body).await
        }
        ScriptAction::HttpError(error) => write_http_error(cx, stream, dialect, &error).await,
        ScriptAction::StreamError(_) | ScriptAction::AbortStream(_) => {
            // These are public extension points for follow-up work. Until their
            // dialect-specific stream renderers exist, fail loudly as a normal
            // provider-shaped HTTP response rather than silently pretending the
            // model completed successfully.
            let error = HttpError::provider(
                501,
                "unsupported_script_action",
                "script action is not implemented by jig-server yet",
            );
            write_http_error(cx, stream, dialect, &error).await
        }
    }
}

fn render_reply(dialect: Dialect, reply: &Reply) -> Vec<SseFrame> {
    match dialect {
        Dialect::OpenAi => render_openai(reply),
        Dialect::Anthropic => render_anthropic(reply),
        Dialect::Codex => render_codex(reply),
    }
}

/// Map a request path to the wire dialect it serves, or `None` for unknown
/// paths (which `404`). The route table is the single source of dialect truth
/// (see bootstrap.md "Why this shape").
fn dialect_for_path(path: &str) -> Option<Dialect> {
    match path {
        "/chat/completions" => Some(Dialect::OpenAi),
        "/v1/messages" => Some(Dialect::Anthropic),
        "/backend-api/codex/responses" => Some(Dialect::Codex),
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

/// An empty Codex view — the fallback when a request body fails to project.
fn empty_codex_view() -> RequestView {
    RequestView::new(Dialect::Codex, None, Vec::new(), 0)
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
async fn read_request(_cx: &Cx, stream: &mut TcpStream) -> io::Result<Request> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];

    // Read until we have the full header block (terminated by CRLFCRLF).
    let header_end = loop {
        if let Some(pos) = find_header_end(&buf) {
            break pos;
        }
        let n = read_some(stream, &mut chunk).await?;
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
        let n = read_some(stream, &mut chunk[..want]).await?;
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

/// Write a non-2xx HTTP error response without SSE or chunked framing.
async fn write_http_error(
    _cx: &Cx,
    stream: &mut TcpStream,
    route_dialect: Dialect,
    error: &HttpError,
) -> io::Result<()> {
    let rendered = error.render_body(route_dialect);
    let content_type = header_value(&error.headers, "content-type")
        .map(str::to_string)
        .unwrap_or(rendered.content_type);
    let body = rendered.body;
    let reason = reason_phrase(error.status);

    let mut response = format!(
        "HTTP/1.1 {} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n",
        error.status,
        body.as_bytes().len()
    );
    for (name, value) in &error.headers {
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

    stream.write_all(response.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        409 => "Conflict",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Error",
    }
}

/// Write a `200` SSE response with the body as a single HTTP/1.1 chunk.
///
/// Chunked transfer-encoding is what real providers use; emitting the whole
/// body as one chunk is sufficient for the SDK parser and keeps the writer
/// trivial. `Connection: close` lets the client treat EOF as end-of-stream.
async fn write_sse_response(_cx: &Cx, stream: &mut TcpStream, body: &str) -> io::Result<()> {
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
async fn write_not_found(_cx: &Cx, stream: &mut TcpStream) -> io::Result<()> {
    let response = "HTTP/1.1 404 Not Found\r\n\
         Content-Length: 0\r\n\
         Connection: close\r\n\
         \r\n";
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}
