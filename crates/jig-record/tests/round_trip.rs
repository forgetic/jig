//! The capture pipeline end to end, all local: a `Recorder` in front of a
//! jig `FakeLlm` (plain HTTP through the upstream hook), then the recording
//! built, written, read back and parsed, and templates derived from it with
//! xtask's `derive`, as `docs/how-to/refresh-fixtures.md` does with a real
//! backend (design `docs/explanation/sans-io-shell.md` §7).
//!
//! Both legs are plain TCP, so a tapped run of the recorder's loop replays
//! byte for byte (§5.8); the last test checks that.

use std::fs;
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;

use jig_core::conform::{DriveShape, ResponseTemplate, strip_rendered_response};
use jig_core::{
    Dialect, Reply, Script, StopReason, Turn, Usage, parse_anthropic_sse, parse_codex_sse,
    parse_openai_sse,
};
use jig_record::{
    Mode, Provenance, Recorder, RecorderConfig, RecorderCore, RecorderHost, RecorderIo, Role,
    UpstreamOverride, build_recording,
};
use jig_server::FakeLlm;
use steploop::reactor::Reactor;
use steploop::run::{IoStep, Tap, replay, run};
use steploop::tls::client_config;

mod support;
use support::*;

const BODY: &str = r#"{"model":"m","stream":true,"messages":[{"role":"user","content":"hi"}]}"#;

fn reply() -> Reply {
    Reply {
        turns: vec![
            Turn::Text("héllo ✓".to_string()),
            Turn::ToolCall {
                id: "call_1".to_string(),
                name: "write".to_string(),
                args: serde_json::json!({ "path": "out.txt" }),
            },
        ],
        usage: Usage {
            prompt_tokens: 7,
            completion_tokens: 9,
        },
        stop: StopReason::ToolCalls,
    }
}

/// A recorder whose fetches go to `fake` in plain HTTP.
fn in_front_of(fake: &FakeLlm, mode: Mode) -> Recorder {
    Recorder::start(RecorderConfig {
        mode,
        upstream: Some(UpstreamOverride {
            addr: addr_of(&fake.base_url()),
            tls: false,
        }),
        ..RecorderConfig::default()
    })
    .unwrap()
}

fn provenance() -> Provenance {
    Provenance {
        client: "round-trip".to_string(),
        role: Role::Authoritative,
        scenario: "tool-call".to_string(),
        client_version: None,
        captured: "2026-09-29".to_string(),
        recorder_sha: "0000000".to_string(),
    }
}

#[test]
fn a_recording_of_jig_parses_back_to_the_scripted_reply() {
    for (dialect, path) in [
        (Dialect::OpenAi, "/chat/completions"),
        (Dialect::Anthropic, "/v1/messages"),
        (Dialect::Codex, "/backend-api/codex/responses"),
    ] {
        let fake = FakeLlm::start(Script::Fixed(reply())).unwrap();
        let rec = in_front_of(&fake, Mode::Once);

        let direct = exchange(addr_of(&fake.base_url()), &post(path, BODY));
        let relayed = exchange(addr_of(&rec.base_url()), &post(path, BODY));
        assert_eq!(relayed, direct, "{dialect:?}: relayed byte for byte");
        let (request, response, route) = rec.next_capture(TIMEOUT).unwrap();
        assert!(direct.ends_with(&response.body), "the raw body is captured");

        let fixtures = tempfile::tempdir().unwrap();
        let recording = build_recording(&request, &response, &route, &provenance());
        let written = recording.write(fixtures.path()).unwrap();
        let sse = fs::read(written.join("response.sse")).unwrap();
        let parsed = match dialect {
            Dialect::OpenAi => parse_openai_sse(&sse).map_err(|e| e.to_string()),
            Dialect::Anthropic => parse_anthropic_sse(&sse).map_err(|e| e.to_string()),
            Dialect::Codex => parse_codex_sse(&sse).map_err(|e| e.to_string()),
        };
        assert_eq!(parsed, Ok(reply()), "{dialect:?}");

        // xtask derives the conformance artifacts from it, and jig's own
        // rendering of the drive shape conforms to them (the T1 check).
        let derived = xtask::derive::derive_tree(fixtures.path()).unwrap();
        let [scenario] = &derived[..] else {
            panic!("{derived:?}");
        };
        let read = |name: &str| fs::read_to_string(scenario.scenario_root.join(name)).unwrap();
        let drive: DriveShape = serde_json::from_str(&read("drive-shape.json")).unwrap();
        assert_eq!(drive.reply, reply(), "{dialect:?}");
        let template: ResponseTemplate =
            serde_json::from_str(&read("response.template.json")).unwrap();
        let stripped = strip_rendered_response(dialect, &drive, &template.headers).unwrap();
        assert_eq!(stripped, template, "{dialect:?}");
    }
}

/// The loop `Recorder` runs, built by hand from the public parts with a
/// `Tap`: a real relay in front of `FakeLlm` replays exactly into fresh
/// steps.
#[test]
fn a_tapped_relay_replays_exactly() {
    let fake = FakeLlm::start(Script::Fixed(reply())).unwrap();
    let upstream = UpstreamOverride {
        addr: addr_of(&fake.base_url()),
        tls: false,
    };
    let fresh = |stop| {
        let io = RecorderIo::new(client_config(None).unwrap(), stop);
        let core = RecorderCore::new(Mode::Once, None, Some(upstream));
        (core, io)
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let mut reactor = Reactor::new().unwrap();
    let (stop_id, _stop) = reactor.signal().unwrap();
    let (mut core, mut io) = fresh(stop_id);
    reactor.adopt_listener(io.listener(), listener).unwrap();
    let (tx, captures) = mpsc::channel();
    let looped = thread::spawn(move || {
        let mut tap = Tap::new();
        let mut host = RecorderHost::new(tx);
        let result = run(&mut core, &mut io, &mut host, &mut reactor, &mut tap);
        (result, tap)
    });

    let got = exchange(addr, &post("/chat/completions", BODY));
    // Once mode ends the loop by itself.
    let (result, tap) = looped.join().unwrap();
    result.unwrap();
    let (_, response, _) = captures.recv().unwrap();
    assert!(got.ends_with(&response.body));

    let (mut core, mut io) = fresh(stop_id);
    let replayed = replay(&mut core, &mut io, &tap);
    assert!(replayed == tap.outputs(), "replay diverged");
    assert!(io.idle());
}
