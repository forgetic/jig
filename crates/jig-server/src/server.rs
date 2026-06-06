//! The async HTTP/1.1 + chunked-SSE server, hand-rolled on a tokio socket.
//!
//! Kept deliberately tiny: this is a single-threaded, low-traffic test double,
//! not a server under load (see bootstrap.md "Runtime & HTTP layer"). We read
//! just enough of each request to route on the path, then stream the rendered
//! SSE frames as an HTTP/1.1 chunked body.

use std::io;
use std::sync::Arc;

use jig_core::{Script, render::frames_to_body, render_openai};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

/// Run the accept loop until `shutdown` fires, then return.
///
/// `listener` is already bound (the caller binds before spawning so `base_url`
/// is valid immediately). Each accepted connection is handled inline — the
/// single-threaded runtime keeps ordering deterministic.
pub async fn serve(
    listener: TcpListener,
    script: Arc<Script>,
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
                        // Handle inline; connections are short-lived SSE streams.
                        if let Err(err) = handle_connection(stream, &script).await {
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

/// Read one request, route it, and stream the response.
async fn handle_connection(mut stream: TcpStream, script: &Script) -> io::Result<()> {
    let request = read_request(&mut stream).await?;

    match request.path.as_str() {
        "/chat/completions" => {
            let reply = script.next_reply();
            let body = frames_to_body(&render_openai(&reply));
            write_sse_response(&mut stream, &body).await
        }
        _ => write_not_found(&mut stream).await,
    }
}

/// A parsed request — only what routing needs.
struct Request {
    path: String,
}

/// Read the request line + headers, then drain any body declared by
/// `Content-Length`. We do not inspect the body in M1, but we must consume it
/// so the socket is in a clean state (and clients that wait for us to read
/// don't stall).
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
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_string();
    // Strip any query string for routing purposes.
    let path = path.split('?').next().unwrap_or("/").to_string();

    let mut content_length = 0usize;
    for line in lines {
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }

    // Drain the body so the socket is clean (we ignore it in M1).
    let body_already_read = buf.len() - (header_end + 4);
    let mut remaining = content_length.saturating_sub(body_already_read);
    while remaining > 0 {
        let want = remaining.min(chunk.len());
        let n = stream.read(&mut chunk[..want]).await?;
        if n == 0 {
            break;
        }
        remaining -= n;
    }

    Ok(Request { path })
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
