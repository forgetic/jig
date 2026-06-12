//! Manual capture harness for the Anthropic (and any) dialect fixtures.
//!
//! NOT part of `cargo test` — this is the online, manual leg the how-to
//! describes. It stands up the passthrough recorder, drives the official client
//! (the `claude` CLI) through it against the *real* backend, and writes the
//! selected routable exchange as an authoritative recording.
//!
//! Why a bespoke harness rather than `record_once`: an agentic `claude -p` run
//! issues *several* `POST /v1/messages` on the way to finishing a task (the
//! tool-call turn, then the tool-result→final turn). `record_once` captures the
//! first routable exchange only. This harness captures *every* routable exchange
//! and lets the operator pick which index to commit (`--capture-index`), so the
//! `tool-result-final` scenario can keep the second POST.
//!
//! Usage:
//!   cargo run -p jig-record --example capture -- \
//!     --scenario tool-result-final --capture-index 1 \
//!     --model claude-sonnet-4-5 --captured 2026-06-07 --recorder-sha <sha> \
//!     --allowed-tools Write,Edit -- <prompt for claude>

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use jig_record::fixture::Recording;
use jig_record::{Provenance, Role, build_recording};

#[path = "shared/pump.rs"]
mod pump;

use pump::CapturePump;

#[derive(Debug)]
struct Args {
    scenario: String,
    client: String,
    capture_index: usize,
    model: String,
    captured: String,
    recorder_sha: String,
    allowed_tools: Option<String>,
    fixtures_root: PathBuf,
    claude_home: Option<String>,
    match_body: Option<String>,
    prompt: String,
}

fn parse_args() -> Args {
    let mut scenario = None;
    let mut client = "claude-code".to_string();
    let mut capture_index = 0usize;
    let mut model = "claude-sonnet-4-5".to_string();
    let mut captured = None;
    let mut recorder_sha = None;
    let mut allowed_tools = None;
    let mut fixtures_root = PathBuf::from("fixtures");
    let mut claude_home = None;
    let mut match_body = None;
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
            "--model" => model = it.next().expect("--model value"),
            "--captured" => captured = it.next(),
            "--recorder-sha" => recorder_sha = it.next(),
            "--allowed-tools" => allowed_tools = it.next(),
            "--fixtures-root" => fixtures_root = PathBuf::from(it.next().expect("value")),
            "--claude-home" => claude_home = it.next(),
            "--match-body" => match_body = it.next(),
            other => panic!("unknown flag {other:?}"),
        }
    }

    Args {
        scenario: scenario.expect("--scenario required"),
        client,
        capture_index,
        model,
        captured: captured.expect("--captured required"),
        recorder_sha: recorder_sha.expect("--recorder-sha required"),
        allowed_tools,
        fixtures_root,
        claude_home,
        match_body,
        prompt: prompt_parts.join(" "),
    }
}

fn main() {
    let args = parse_args();

    // Capture every routable exchange while `claude` runs, accepting
    // connections concurrently on the pump's own runtime thread (see
    // `shared/pump.rs` for why concurrency matters here).
    let pump = CapturePump::start(None).expect("start capture pump");
    let base_url = pump.base_url();
    eprintln!("recorder listening at {base_url}");

    // Drive the claude CLI through the recorder against the real backend.
    //
    // Run it in an **isolated** HOME (`--claude-home`) holding only the OAuth
    // credentials, from an empty scratch dir, so the captured request is the
    // faithful Claude Code wire shape — the mandatory system block + identity
    // headers — without this machine's local skills, MCP servers, or project
    // CLAUDE.md bloating the body. `env -i`-style: we clear the environment and
    // pass only what claude needs.
    let real_home = std::env::var("HOME").unwrap();
    let claude = format!("{real_home}/.local/bin/claude");
    let claude_home = args
        .claude_home
        .clone()
        .unwrap_or_else(|| real_home.clone());

    let mut cmd = Command::new(&claude);
    cmd.arg("-p").arg("--dangerously-skip-permissions");
    if let Some(tools) = &args.allowed_tools {
        // `--allowed-tools` is variadic, so it must NOT be the last flag before a
        // positional — it would swallow the prompt. We feed the prompt on stdin
        // instead (claude `-p` reads it), so flag order is unambiguous.
        cmd.arg("--allowed-tools").arg(tools);
    }
    cmd.arg("--model").arg(&args.model);
    // Prompt via stdin (see above): write it after spawn.
    cmd.stdin(std::process::Stdio::piped());

    // Run claude in a scratch dir so any files it writes do not touch the repo.
    let scratch = std::env::temp_dir().join(format!("jig-capture-{}", args.scenario));
    let _ = std::fs::remove_dir_all(&scratch);
    let _ = std::fs::create_dir_all(&scratch);
    cmd.current_dir(&scratch);

    // Minimal, controlled environment.
    cmd.env_clear()
        .env("HOME", &claude_home)
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("ANTHROPIC_BASE_URL", &base_url);

    eprintln!(
        "running: claude -p --model {} (scenario {})",
        args.model, args.scenario
    );
    let mut child = cmd.spawn().expect("spawn claude");
    {
        use std::io::Write as _;
        let mut stdin = child.stdin.take().expect("claude stdin");
        stdin
            .write_all(args.prompt.as_bytes())
            .expect("write prompt to claude stdin");
        // Dropping stdin closes it, signalling EOF so claude -p proceeds.
    }
    let status = child.wait().expect("wait claude");
    eprintln!("claude exited: {status}");

    // Give the pump a beat to record the final exchange, then stop it.
    std::thread::sleep(std::time::Duration::from_millis(300));
    let exchanges = pump.stop();
    eprintln!("total routable exchanges captured: {}", exchanges.len());
    if exchanges.is_empty() {
        eprintln!("ERROR: no routable exchanges captured");
        std::process::exit(1);
    }

    // Pick the exchange to commit. Capture is concurrent and Claude Code also
    // fires internal housekeeping calls (e.g. session-title generation), so we
    // do not trust raw index order. Filter to the exchanges whose request body
    // contains `--match-body` (a unique marker placed in the prompt), then take
    // the `--capture-index`-th of *those* — index 0 is the first matching turn,
    // and the tool-result→final scenario takes a later turn.
    let selected: Vec<usize> = exchanges
        .iter()
        .enumerate()
        .filter(|(_, (req, _, _))| {
            let body = String::from_utf8_lossy(&req.body);
            // Drop Claude Code's internal session-title housekeeping call — it is
            // not the scenario, it just summarizes the prompt for the session list.
            if body.contains("Generate a concise, sentence-case title") {
                return false;
            }
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
