//! A tiny blocking HTTP/1.1 test client on `std::net`, replacing `reqwest`.
//!
//! jig's tests drive a loopback `FakeLlm` with two shapes only: POST a JSON
//! body and read the response, or GET a path and read the status line.
//! Hand-rolling those ~100 lines keeps the dev-dependency tree free of an
//! embedded async runtime — the same spirit as jig's own hand-rolled server.

// Each integration-test binary compiles its own copy of this module and uses
// only a subset of the helpers, so per-binary dead-code warnings are noise.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::TcpStream;

/// Parsed blocking HTTP response from the fake server.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Response {
    /// Return a header value by case-insensitive name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// POST `body` as JSON to `url` (e.g. `http://127.0.0.1:PORT/path`) with the
/// extra `headers`, returning the response body (de-chunked if the response
/// uses chunked transfer-encoding).
pub fn post_json(url: &str, headers: &[(&str, &str)], body: &serde_json::Value) -> String {
    post_json_response(url, headers, body).body
}

/// POST `body` as JSON to `url`, returning status, headers, and body.
pub fn post_json_response(
    url: &str,
    headers: &[(&str, &str)],
    body: &serde_json::Value,
) -> Response {
    let (authority, path) = split_url(url);
    let payload = serde_json::to_vec(body).expect("serialize request body");

    let mut request = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {authority}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n",
        payload.len()
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");

    let response = exchange(&authority, request.as_bytes(), &payload);
    parse_response(&response)
}

/// GET `url`, returning the response status code.
pub fn get_status(url: &str) -> u16 {
    let (authority, path) = split_url(url);
    let request = format!("GET {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    let response = exchange(&authority, request.as_bytes(), &[]);
    parse_response(&response).status
}

/// Split `http://host:port/path` into (`host:port`, `/path`).
fn split_url(url: &str) -> (String, String) {
    let rest = url.strip_prefix("http://").expect("http:// url");
    match rest.split_once('/') {
        Some((authority, path)) => (authority.to_string(), format!("/{path}")),
        None => (rest.to_string(), "/".to_string()),
    }
}

/// Write the request head + body, then read the whole response until EOF
/// (every jig response is `Connection: close`).
fn exchange(authority: &str, head: &[u8], body: &[u8]) -> Vec<u8> {
    let mut stream = TcpStream::connect(authority).expect("connect to FakeLlm");
    stream.write_all(head).expect("write request head");
    stream.write_all(body).expect("write request body");
    stream.flush().expect("flush request");
    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    response
}

/// Parse raw response bytes, de-chunking the body when needed.
fn parse_response(response: &[u8]) -> Response {
    let header_end = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response has a header block");
    let head = String::from_utf8_lossy(&response[..header_end]);
    let body = &response[header_end + 4..];

    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .expect("status code in response");
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_string(), value.trim().to_string()))
        })
        .collect();

    let chunked = headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("transfer-encoding") && value.eq_ignore_ascii_case("chunked")
    });
    let body = if chunked {
        dechunk(body)
    } else {
        body.to_vec()
    };

    Response {
        status,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

/// De-chunk repeated "<hex len>\r\n<data>\r\n" until the "0\r\n\r\n" end.
fn dechunk(mut rest: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let line_end = rest
            .windows(2)
            .position(|w| w == b"\r\n")
            .expect("chunk size line");
        let size_text = String::from_utf8_lossy(&rest[..line_end]);
        let size = usize::from_str_radix(size_text.trim(), 16).expect("hex chunk size");
        rest = &rest[line_end + 2..];
        if size == 0 {
            break;
        }
        out.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..]; // skip the chunk's trailing CRLF
    }
    out
}
