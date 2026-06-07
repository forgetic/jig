//! Self-contained capture harness for the OpenAI/DeepSeek (chat-completions)
//! dialect fixtures.
//!
//! NOT part of `cargo test` — this is the online, manual leg the how-to
//! describes, the OpenAI counterpart of the agentic `capture` (claude) and
//! `codex_capture` examples. Unlike those, the chat-completions request is a
//! single, deterministic exchange with no external CLI to drive, so this harness
//! issues the request **itself**: it stands up the passthrough recorder and
//! sends one scenario request straight at the recorder's loopback `base_url`,
//! capturing the forwarded exchange. That makes
//! `xtask record --dialect openai` fully unattended.
//!
//! The bearer is read from the environment so no key ever touches the source or
//! a fixture — `DEEPSEEK_API_KEY` when `--upstream-host api.deepseek.com` is set
//! (the recommended OpenAI-compatible backend), otherwise `OPENAI_API_KEY`.
//!
//! Usage:
//!   cargo run -p jig-record --example openai_capture -- \
//!     --scenario tool-call --captured "$(date -u +%F)" \
//!     --recorder-sha "$(git rev-parse --short HEAD)" \
//!     --upstream-host api.deepseek.com

use std::io::Write;
use std::path::{Path, PathBuf};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use jig_record::fixture::Recording;
use jig_record::proxy::{bind, handle_connection};
use jig_record::{Provenance, Role, build_recording};

#[derive(Debug)]
struct Args {
    scenario: String,
    client: String,
    captured: String,
    recorder_sha: String,
    fixtures_root: PathBuf,
    upstream_host: Option<String>,
    /// Model id named in the request body. Defaults to a current DeepSeek chat
    /// model when an `--upstream-host` is set, else a cheap OpenAI model. T3
    /// masks the requested model, so this only has to satisfy the live backend.
    model: Option<String>,
}

fn parse_args() -> Args {
    let mut scenario = None;
    let mut client = "openai-sdk".to_string();
    let mut captured = None;
    let mut recorder_sha = None;
    let mut fixtures_root = PathBuf::from("fixtures");
    let mut upstream_host = None;
    let mut model = None;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--scenario" => scenario = it.next(),
            "--client" => client = it.next().expect("--client value"),
            "--captured" => captured = it.next(),
            "--recorder-sha" => recorder_sha = it.next(),
            "--fixtures-root" => fixtures_root = PathBuf::from(it.next().expect("value")),
            "--upstream-host" => upstream_host = it.next(),
            "--model" => model = it.next(),
            other => panic!("unknown flag {other:?}"),
        }
    }

    Args {
        scenario: scenario.expect("--scenario required"),
        client,
        captured: captured.expect("--captured required"),
        recorder_sha: recorder_sha.expect("--recorder-sha required"),
        fixtures_root,
        upstream_host,
        model,
    }
}

/// The default model id for a given upstream. DeepSeek's current cheap chat
/// model when recording against DeepSeek; a cheap OpenAI model otherwise. T3
/// masks the requested model, so this only has to satisfy the live backend —
/// keep it in step with `Scenario`'s subject model in
/// `crates/jig-oracle/tests/support/subject.rs`.
fn default_model(upstream_host: Option<&str>) -> &'static str {
    match upstream_host {
        // `deepseek-chat` was retired; `deepseek-v4-flash` is the current cheapest.
        Some(h) if h.contains("deepseek") => "deepseek-v4-flash",
        _ => "gpt-4o-mini",
    }
}

/// Read the bearer for the chosen upstream from the environment. DeepSeek key
/// when recording against DeepSeek, otherwise the OpenAI key. The recorder
/// forwards it on the wire and redacts the captured copy, so it never lands in a
/// fixture.
fn resolve_bearer(upstream_host: Option<&str>) -> String {
    let deepseek = matches!(upstream_host, Some(h) if h.contains("deepseek"));
    let (var, alt) = if deepseek {
        ("DEEPSEEK_API_KEY", "OPENAI_API_KEY")
    } else {
        ("OPENAI_API_KEY", "DEEPSEEK_API_KEY")
    };
    std::env::var(var)
        .or_else(|_| std::env::var(alt))
        .unwrap_or_else(|_| {
            eprintln!(
                "ERROR: no API key in ${var} (or ${alt}). \
                 Export the bearer for the backend you are recording against."
            );
            std::process::exit(1);
        })
}

/// Build the chat-completions request body for a scenario. Streaming, with
/// `stream_options.include_usage` so the capture carries the final usage frame,
/// exactly as the documented operator request does.
fn scenario_body(scenario: &str, model: &str) -> serde_json::Value {
    let weather_tool = serde_json::json!({
        "type": "function",
        "function": {
            "name": "get_weather",
            "description": "Get current weather for a city",
            "parameters": {
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"]
            }
        }
    });

    let base = serde_json::json!({
        "model": model,
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    let mut body = base.as_object().unwrap().clone();

    match scenario {
        "single-text" => {
            body.insert(
                "messages".to_string(),
                serde_json::json!([
                    { "role": "system", "content": "You are a terse test assistant." },
                    { "role": "user", "content": "Reply with exactly: hello" }
                ]),
            );
        }
        "tool-call" => {
            body.insert("tools".to_string(), serde_json::json!([weather_tool]));
            body.insert("tool_choice".to_string(), serde_json::json!("required"));
            body.insert(
                "messages".to_string(),
                serde_json::json!([
                    { "role": "user", "content": "What is the weather in Paris? Call the get_weather tool." }
                ]),
            );
        }
        "parallel-tool-calls" => {
            body.insert("tools".to_string(), serde_json::json!([weather_tool]));
            body.insert("tool_choice".to_string(), serde_json::json!("required"));
            body.insert(
                "messages".to_string(),
                serde_json::json!([
                    { "role": "user", "content": "Get the current weather for both Paris and London. Call the get_weather tool once for each city, in the same turn." }
                ]),
            );
        }
        "tool-result-final" => {
            // The prior assistant tool call + its result, fed back so the capture
            // is the multi-turn request grammar ending in a final text reply.
            body.insert("tools".to_string(), serde_json::json!([weather_tool]));
            body.insert(
                "messages".to_string(),
                serde_json::json!([
                    { "role": "user", "content": "What is the weather in Paris?" },
                    {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_jig_openai_1",
                            "type": "function",
                            "function": { "name": "get_weather", "arguments": "{\"city\":\"Paris\"}" }
                        }]
                    },
                    { "role": "tool", "tool_call_id": "call_jig_openai_1", "content": "sunny, 24C" },
                    { "role": "user", "content": "Now tell me the weather in one short sentence." }
                ]),
            );
        }
        other => {
            eprintln!("ERROR: unknown scenario {other:?}");
            std::process::exit(1);
        }
    }

    serde_json::Value::Object(body)
}

/// Issue one plain-HTTP `POST /chat/completions` at the recorder's loopback
/// `base_url`, draining the response so the full exchange round-trips through
/// the proxy. The recorder forwards it to the real upstream over HTTPS.
async fn drive_request(
    addr: std::net::SocketAddr,
    bearer: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let mut stream = TcpStream::connect(addr).await?;
    let head = format!(
        "POST /chat/completions HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Authorization: Bearer {bearer}\r\n\
         Content-Type: application/json\r\n\
         Accept: text/event-stream\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await?;

    // Drain the response to EOF so the recorder captures the whole stream.
    let mut sink = Vec::new();
    stream.read_to_end(&mut sink).await?;
    Ok(())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let args = parse_args();

    let listener = bind().await.expect("bind recorder");
    let addr = listener.local_addr().expect("local addr");
    eprintln!("recorder listening at http://{addr}");

    let upstream = args.upstream_host.clone();
    let model = args
        .model
        .clone()
        .unwrap_or_else(|| default_model(upstream.as_deref()).to_string());
    let bearer = resolve_bearer(upstream.as_deref());
    let body = serde_json::to_vec(&scenario_body(&args.scenario, &model)).expect("serialize body");

    // Accept the one routable connection in the background while we drive the
    // request. The recorder answers any non-routable preflight with 204; the
    // chat-completions POST is the capture.
    let upstream_for_accept = upstream.clone();
    let accept = tokio::spawn(async move {
        loop {
            let (client, _peer) = listener.accept().await.expect("accept");
            match handle_connection(client, upstream_for_accept.as_deref()).await {
                Ok(Some(triple)) => return triple,
                Ok(None) => continue, // preflight, keep waiting for the real POST
                Err(e) => panic!("connection error: {e}"),
            }
        }
    });

    eprintln!(
        "driving {} request (model {}, upstream {})",
        args.scenario,
        model,
        upstream.as_deref().unwrap_or("api.openai.com")
    );
    if let Err(e) = drive_request(addr, &bearer, &body).await {
        eprintln!("ERROR driving request: {e}");
        std::process::exit(1);
    }

    let (request, response, route) = accept.await.expect("accept task panicked");
    eprintln!(
        "captured {} {} -> {} ({} body bytes)",
        request.method,
        request.path(),
        response.status,
        response.body.len()
    );
    if !(200..300).contains(&response.status) {
        eprintln!(
            "WARNING: upstream returned HTTP {} — the capture is written but is a finding, \
             not a clean fixture. Body:\n{}",
            response.status,
            String::from_utf8_lossy(&response.body)
        );
    }

    let provenance = Provenance {
        client: args.client.clone(),
        role: Role::Authoritative,
        scenario: args.scenario.clone(),
        client_version: Some("curl-equivalent".to_string()),
        captured: args.captured.clone(),
        recorder_sha: args.recorder_sha.clone(),
    };

    let recording: Recording = build_recording(&request, &response, &route, &provenance);
    let written = recording
        .write(Path::new(&args.fixtures_root))
        .expect("write recording");
    println!("wrote recording to {}", written.display());

    // Echo the captured event kinds so the operator can eyeball the shape.
    let body = String::from_utf8_lossy(&response.body);
    let kinds: Vec<&str> = body
        .lines()
        .filter(|l| l.starts_with("data: "))
        .take(3)
        .collect();
    eprintln!("first response frames: {kinds:?}");
    let _ = std::io::stderr().flush();

    // Non-2xx is a finding; exit non-zero so the orchestrator records the failure.
    if !(200..300).contains(&response.status) {
        std::process::exit(1);
    }
}
