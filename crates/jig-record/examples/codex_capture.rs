//! Manual capture harness for the Codex (`responses`) dialect fixtures.
//!
//! NOT part of `cargo test` — this is the online, manual leg the how-to
//! describes, the Codex counterpart of the `capture` example (which drives the
//! `claude` CLI). It stands up the passthrough recorder, drives the official
//! **Codex CLI** (`codex exec`) through it against the *real* ChatGPT backend,
//! and writes the selected routable exchange as an authoritative recording.
//!
//! Why a bespoke harness rather than `record_once`: an agentic `codex exec` run
//! issues *several* `POST /backend-api/codex/responses` on the way to finishing
//! a task (the tool-call turn, then the tool-result→final turn). `record_once`
//! captures the first routable exchange only. This harness captures *every*
//! routable exchange and lets the operator pick which index to commit
//! (`--capture-index`), so the `tool-result-final` scenario can keep a later
//! POST.
//!
//! Codex uses its own auth (`~/.codex/auth.json`, ChatGPT OAuth) — there is no
//! API key to thread through. We point it at the recorder with a custom model
//! provider whose `base_url` is the loopback recorder and whose `wire_api` is
//! `responses`, exactly as issue #16 documents:
//!
//!   cargo run -p jig-record --example codex_capture -- \
//!     --scenario tool-result-final --capture-index 1 --match-body JIGTOOL42 \
//!     --captured "$(date -u +%F)" --recorder-sha "$(git rev-parse --short HEAD)" \
//!     -- "JIGTOOL42 Create a file named greeting.txt containing exactly: bar"

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use jig_record::fixture::Recording;
use jig_record::proxy::{bind, handle_connection};
use jig_record::{Provenance, Role, build_recording};

#[derive(Debug)]
struct Args {
    scenario: String,
    client: String,
    capture_index: usize,
    captured: String,
    recorder_sha: String,
    fixtures_root: PathBuf,
    match_body: Option<String>,
    /// The path segment the custom provider's `base_url` ends in. Codex appends
    /// `/responses` to the provider `base_url`, so the recorder must be reached
    /// at a `base_url` whose suffix + `/responses` is the routable
    /// `/backend-api/codex/responses`. Default matches the issue.
    base_path: String,
    /// Extra `-c key=value` config overrides passed to `codex exec` verbatim.
    /// Used to enable reasoning for the thinking-text scenario
    /// (`--config model_reasoning_effort=high
    /// --config model_reasoning_summary=detailed`).
    config: Vec<String>,
    prompt: String,
}

fn parse_args() -> Args {
    let mut scenario = None;
    let mut client = "codex".to_string();
    let mut capture_index = 0usize;
    let mut captured = None;
    let mut recorder_sha = None;
    let mut fixtures_root = PathBuf::from("fixtures");
    let mut match_body = None;
    let mut base_path = "/backend-api/codex".to_string();
    let mut config: Vec<String> = Vec::new();
    let mut prompt_parts: Vec<String> = Vec::new();

    let mut it = std::env::args().skip(1);
    let mut in_prompt = false;
    while let Some(arg) = it.next() {
        if in_prompt {
            prompt_parts.push(arg);
            continue;
        }
        match arg.as_str() {
            "--" => in_prompt = true,
            "--scenario" => scenario = it.next(),
            "--client" => client = it.next().expect("--client value"),
            "--capture-index" => {
                capture_index = it.next().expect("--capture-index value").parse().unwrap()
            }
            "--captured" => captured = it.next(),
            "--recorder-sha" => recorder_sha = it.next(),
            "--fixtures-root" => fixtures_root = PathBuf::from(it.next().expect("value")),
            "--match-body" => match_body = it.next(),
            "--base-path" => base_path = it.next().expect("--base-path value"),
            "--config" => config.push(it.next().expect("--config value")),
            other => panic!("unknown flag {other:?}"),
        }
    }

    Args {
        scenario: scenario.expect("--scenario required"),
        client,
        capture_index,
        captured: captured.expect("--captured required"),
        recorder_sha: recorder_sha.expect("--recorder-sha required"),
        fixtures_root,
        match_body,
        base_path,
        config,
        prompt: prompt_parts.join(" "),
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let args = parse_args();

    let listener = bind().await.expect("bind recorder");
    let addr = listener.local_addr().expect("local addr");
    let base_url = format!("http://{addr}{}", args.base_path);
    eprintln!("recorder listening; provider base_url = {base_url}");

    // Capture every routable exchange while `codex` runs. Real clients pre-open a
    // pool of connections and send the request on one of them, so we accept
    // connections **concurrently** — one task per connection — rather than
    // serially (see the `capture` example for the same rationale).
    let captured: Arc<
        Mutex<
            Vec<(
                jig_record::ClientRequest,
                jig_record::UpstreamResponse,
                jig_record::Route,
            )>,
        >,
    > = Arc::new(Mutex::new(Vec::new()));
    let captured_bg = Arc::clone(&captured);
    let pump = tokio::spawn(async move {
        loop {
            let (client, _peer) = match listener.accept().await {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("accept ended: {e}");
                    break;
                }
            };
            let captured_conn = Arc::clone(&captured_bg);
            tokio::spawn(async move {
                match handle_connection(client, None).await {
                    Ok(Some(triple)) => {
                        let n = {
                            let mut g = captured_conn.lock().unwrap();
                            g.push(triple);
                            g.len() - 1
                        };
                        let g = captured_conn.lock().unwrap();
                        let (req, resp, _) = &g[n];
                        eprintln!(
                            "captured exchange #{n} {} {} -> {} ({} body bytes)",
                            req.method,
                            req.path(),
                            resp.status,
                            resp.body.len()
                        );
                    }
                    Ok(None) => {} // preflight, answered with 204
                    Err(e) => eprintln!("connection error: {e}"),
                }
            });
        }
    });

    // Drive the Codex CLI through the recorder against the real backend.
    //
    // `codex exec` is non-interactive; `--dangerously-bypass-approvals-and-sandbox`
    // avoids all approval prompts; `--skip-git-repo-check` lets it run in an
    // arbitrary scratch dir. A custom `jig` model provider points it at the
    // recorder with `wire_api=responses` and `requires_openai_auth=true` so the
    // ChatGPT OAuth bearer is sent (and forwarded verbatim upstream).
    let real_home = std::env::var("HOME").unwrap();
    let codex = format!("{real_home}/node_modules/.bin/codex");

    // Run codex in a scratch dir so any files it writes do not touch the repo.
    let scratch = std::env::temp_dir().join(format!("jig-codex-capture-{}", args.scenario));
    let _ = std::fs::remove_dir_all(&scratch);
    let _ = std::fs::create_dir_all(&scratch);

    let mut cmd = Command::new(&codex);
    cmd.arg("exec")
        .arg("--dangerously-bypass-approvals-and-sandbox")
        .arg("--skip-git-repo-check")
        .arg("-C")
        .arg(&scratch)
        .arg("-c")
        .arg("model_provider=jig")
        .arg("-c")
        .arg("model_providers.jig.name=jig")
        .arg("-c")
        .arg(format!("model_providers.jig.base_url=\"{base_url}\""))
        .arg("-c")
        .arg("model_providers.jig.wire_api=\"responses\"")
        .arg("-c")
        .arg("model_providers.jig.requires_openai_auth=true");
    // Extra config overrides (e.g. reasoning effort for the thinking-text
    // scenario), passed verbatim before the positional prompt.
    for kv in &args.config {
        cmd.arg("-c").arg(kv);
    }
    cmd.arg(&args.prompt);
    cmd.current_dir(&scratch);

    // Keep the inherited environment (codex reads ~/.codex/auth.json under HOME).
    eprintln!("running: codex exec (scenario {})", args.scenario);
    let status = cmd.status().expect("spawn codex");
    eprintln!("codex exited: {status}");

    // Give the pump a beat to record the final exchange, then stop it.
    std::thread::sleep(std::time::Duration::from_millis(500));
    pump.abort();

    let exchanges = captured.lock().unwrap();
    eprintln!("total routable exchanges captured: {}", exchanges.len());
    if exchanges.is_empty() {
        eprintln!("ERROR: no routable exchanges captured");
        std::process::exit(1);
    }

    // Pick the exchange to commit. Capture is concurrent, so we do not trust raw
    // index order. Filter to the exchanges whose request body contains
    // `--match-body` (a unique marker placed in the prompt), then take the
    // `--capture-index`-th of *those* — index 0 is the first matching turn, and
    // the tool-result→final scenario takes a later turn.
    let selected: Vec<usize> = exchanges
        .iter()
        .enumerate()
        .filter(|(_, (req, _, _))| {
            let body = String::from_utf8_lossy(&req.body);
            match &args.match_body {
                Some(needle) => body.contains(needle.as_str()),
                None => true,
            }
        })
        .map(|(i, _)| i)
        .collect();
    if selected.is_empty() {
        eprintln!(
            "ERROR: no captured exchange matched --match-body {:?}",
            args.match_body
        );
        std::process::exit(1);
    }
    eprintln!("matching exchanges (indices): {selected:?}");
    let pick = *selected
        .get(args.capture_index)
        .unwrap_or(selected.last().unwrap());
    let (request, response, route) = &exchanges[pick];
    eprintln!(
        "committing exchange #{pick}: {} {} -> {} ({} body bytes)",
        request.method,
        request.path(),
        response.status,
        response.body.len()
    );

    let provenance = Provenance {
        client: args.client.clone(),
        role: Role::Authoritative,
        scenario: args.scenario.clone(),
        client_version: None,
        captured: args.captured.clone(),
        recorder_sha: args.recorder_sha.clone(),
    };

    let recording: Recording = build_recording(request, response, route, &provenance);
    let written = recording
        .write(Path::new(&args.fixtures_root))
        .expect("write recording");
    println!("wrote recording to {}", written.display());

    // Echo a short summary of the captured response so the operator can eyeball
    // the scenario shape without opening the file.
    let body = String::from_utf8_lossy(&response.body);
    let kinds: Vec<&str> = body
        .lines()
        .filter_map(|l| l.strip_prefix("event: "))
        .collect();
    eprintln!("response events: {kinds:?}");
    let _ = std::io::stderr().flush();
}
