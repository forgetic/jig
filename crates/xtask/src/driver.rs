//! Per-cell **driver dispatch**: how each `(dialect, client)` cell is actually
//! recorded.
//!
//! P5 (#19) promised a *one-command* refresh: `xtask record --all` should start
//! the recorder, drive the relevant official client (or the pi-SDK subject
//! harness) end to end, and leave the fixture tree refreshed — not just spawn a
//! bare `jig record` per cell and wait for a human to drive the client by hand.
//!
//! This module is the table that turns a [`RecordInvocation`] into the concrete
//! command that records it:
//!
//! - **openai-sdk** (authoritative) drives the recorder with a deterministic,
//!   self-contained HTTP request via the `openai_capture` example — no external
//!   client needed, and `--upstream-host api.deepseek.com` records against
//!   DeepSeek.
//! - **claude-code** (authoritative) reuses the `capture` example: the agentic
//!   `claude` CLI harness, with the per-scenario prompt/marker/tools baked in
//!   here so the matrix stays the single source of truth.
//! - **codex** (authoritative) reuses the `codex_capture` example: the agentic
//!   `codex exec` harness, with the per-scenario prompt/marker/config baked in.
//! - **pi-sdk** (subject) drives the factored pi-SDK harness via the
//!   `pi_subject_record` example — the same logic the `#[ignore]`d test used,
//!   now callable from xtask without `cargo test -- --ignored`.
//!
//! Like [`crate::matrix`], this is **pure**: [`driver_for`] is a data transform
//! from an invocation to a [`DriverCommand`] (program + argv + a one-line dry-run
//! description). The only impure leg — actually spawning the command — lives in
//! `main`, so the whole dispatch table is unit-tested offline.

use crate::Provenance;
use crate::matrix::{RecordInvocation, Role};

/// A concrete command that records one matrix cell, ready to spawn.
///
/// `program` is the executable (always `cargo`, re-invoked to build+run an
/// example), `args` is its full argv, and `describe` is the human-readable line
/// `--dry-run` prints so the operator sees exactly what each cell will do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverCommand {
    /// The executable to spawn (the cargo that re-invokes the workspace).
    pub program: String,
    /// The full argument vector passed to `program`.
    pub args: Vec<String>,
    /// A one-line, human-readable summary of the action for `--dry-run`.
    pub describe: String,
}

/// The cargo executable to re-invoke, honoring `$CARGO` when xtask is itself run
/// under cargo (the usual case), falling back to `cargo` on `PATH`. Kept here so
/// the rendered [`DriverCommand`] is fully self-describing.
pub fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

/// Build the [`DriverCommand`] that records `inv`, stamping `provenance` and
/// writing under `fixtures_root`.
///
/// Dispatches on `(role, client)`: official clients (`authoritative`) each have a
/// bespoke `jig-record` example harness; the pi-SDK (`subject`) has the factored
/// `jig-oracle` example. An unknown client falls back to the generic
/// `jig record` spawn, preserving the pre-#19 behaviour for any cell not yet
/// taught a real driver.
pub fn driver_for(
    inv: &RecordInvocation,
    fixtures_root: &str,
    provenance: &Provenance,
) -> DriverCommand {
    match (inv.role, inv.client.as_str()) {
        (Role::Subject, "pi-sdk") => pi_subject_driver(inv, fixtures_root, provenance),
        (Role::Authoritative, "openai-sdk") => openai_driver(inv, fixtures_root, provenance),
        (Role::Authoritative, "claude-code") => claude_driver(inv, fixtures_root, provenance),
        (Role::Authoritative, "codex") => codex_driver(inv, fixtures_root, provenance),
        // Any client without a bespoke harness: drive bare `jig record`, exactly
        // as xtask did before #19. The operator points the client at the printed
        // loopback base_url by hand. This keeps an unrecognised matrix entry
        // working rather than silently unrecordable.
        _ => generic_jig_record_driver(inv, fixtures_root, provenance),
    }
}

/// Common leading argv for `cargo run -p <pkg> --example <example> --`.
fn cargo_example(pkg: &str, example: &str) -> Vec<String> {
    vec![
        "run".to_string(),
        "--quiet".to_string(),
        "-p".to_string(),
        pkg.to_string(),
        "--example".to_string(),
        example.to_string(),
        "--".to_string(),
    ]
}

/// Append the shared provenance flags (`--captured`, `--recorder-sha`) every
/// example harness accepts.
fn push_provenance(args: &mut Vec<String>, provenance: &Provenance) {
    args.push("--captured".to_string());
    args.push(provenance.captured.clone());
    args.push("--recorder-sha".to_string());
    args.push(provenance.recorder_sha.clone());
}

/// **openai-sdk authoritative** → the self-contained `openai_capture` example.
///
/// No external client is required: the example binds the recorder and issues the
/// scenario's chat-completions request itself, so `record --dialect openai` works
/// unattended. The upstream-host override (DeepSeek) is threaded through.
fn openai_driver(
    inv: &RecordInvocation,
    fixtures_root: &str,
    provenance: &Provenance,
) -> DriverCommand {
    let mut args = cargo_example("jig-record", "openai_capture");
    args.push("--scenario".to_string());
    args.push(inv.scenario.clone());
    args.push("--client".to_string());
    args.push(inv.client.clone());
    args.push("--fixtures-root".to_string());
    args.push(fixtures_root.to_string());
    push_provenance(&mut args, provenance);
    if let Some(host) = &provenance.upstream_host {
        args.push("--upstream-host".to_string());
        args.push(host.clone());
    }
    let describe = format!(
        "openai_capture example: deterministic {} request {}",
        inv.scenario,
        match &provenance.upstream_host {
            Some(h) => format!("against {h}"),
            None => "against api.openai.com".to_string(),
        }
    );
    DriverCommand {
        program: cargo(),
        args,
        describe,
    }
}

/// **claude-code authoritative** → the agentic `capture` example driving the
/// `claude` CLI, with the per-scenario prompt/marker/tools/index baked in.
fn claude_driver(
    inv: &RecordInvocation,
    fixtures_root: &str,
    provenance: &Provenance,
) -> DriverCommand {
    let plan = ClaudePlan::for_scenario(&inv.scenario);
    let mut args = cargo_example("jig-record", "capture");
    args.push("--scenario".to_string());
    args.push(inv.scenario.clone());
    args.push("--client".to_string());
    args.push(inv.client.clone());
    args.push("--capture-index".to_string());
    args.push(plan.capture_index.to_string());
    args.push("--match-body".to_string());
    args.push(plan.marker.to_string());
    args.push("--model".to_string());
    args.push(plan.model.to_string());
    if let Some(tools) = plan.allowed_tools {
        args.push("--allowed-tools".to_string());
        args.push(tools.to_string());
    }
    args.push("--fixtures-root".to_string());
    args.push(fixtures_root.to_string());
    push_provenance(&mut args, provenance);
    // Isolate HOME so the captured request is the faithful Claude Code wire
    // shape, not bloated by this machine's skills/MCP/CLAUDE.md.
    args.push("--claude-home".to_string());
    args.push("/tmp/jig-claude-home".to_string());
    args.push("--".to_string());
    args.push(plan.prompt.to_string());
    let describe = format!(
        "capture example: claude CLI, scenario {}, marker {}, capture-index {}",
        inv.scenario, plan.marker, plan.capture_index
    );
    DriverCommand {
        program: cargo(),
        args,
        describe,
    }
}

/// **codex authoritative** → the agentic `codex_capture` example driving the
/// `codex exec` CLI, with the per-scenario prompt/marker/config baked in.
fn codex_driver(
    inv: &RecordInvocation,
    fixtures_root: &str,
    provenance: &Provenance,
) -> DriverCommand {
    let plan = CodexPlan::for_scenario(&inv.scenario);
    let mut args = cargo_example("jig-record", "codex_capture");
    args.push("--scenario".to_string());
    args.push(inv.scenario.clone());
    args.push("--client".to_string());
    args.push(inv.client.clone());
    args.push("--capture-index".to_string());
    args.push(plan.capture_index.to_string());
    args.push("--match-body".to_string());
    args.push(plan.marker.to_string());
    for kv in plan.config {
        args.push("--config".to_string());
        args.push(kv.to_string());
    }
    args.push("--fixtures-root".to_string());
    args.push(fixtures_root.to_string());
    push_provenance(&mut args, provenance);
    args.push("--".to_string());
    args.push(plan.prompt.to_string());
    let describe = format!(
        "codex_capture example: codex exec, scenario {}, marker {}, capture-index {}",
        inv.scenario, plan.marker, plan.capture_index
    );
    DriverCommand {
        program: cargo(),
        args,
        describe,
    }
}

/// **pi-sdk subject** → the factored `pi_subject_record` example, selecting the
/// cell by `--dialect`/`--scenario` (the same logic the ignored test ran).
fn pi_subject_driver(
    inv: &RecordInvocation,
    fixtures_root: &str,
    provenance: &Provenance,
) -> DriverCommand {
    let mut args = cargo_example("jig-oracle", "pi_subject_record");
    args.push("--dialect".to_string());
    args.push(inv.dialect.clone());
    args.push("--scenario".to_string());
    args.push(inv.scenario.clone());
    args.push("--fixtures-root".to_string());
    args.push(fixtures_root.to_string());
    push_provenance(&mut args, provenance);
    let describe = format!(
        "pi_subject_record example: pi-SDK subject, {}/{}",
        inv.dialect, inv.scenario
    );
    DriverCommand {
        program: cargo(),
        args,
        describe,
    }
}

/// Generic fallback: spawn `jig record` and let the operator drive the client by
/// hand at the printed loopback base_url. Pre-#19 behaviour, kept for any cell
/// without a bespoke harness.
fn generic_jig_record_driver(
    inv: &RecordInvocation,
    fixtures_root: &str,
    provenance: &Provenance,
) -> DriverCommand {
    let mut args = vec![
        "run".to_string(),
        "--quiet".to_string(),
        "--bin".to_string(),
        "jig".to_string(),
        "--".to_string(),
    ];
    args.extend(inv.argv(fixtures_root, provenance));
    let describe = format!(
        "jig record (manual): start the recorder for {}/{} via {} and drive the client by hand",
        inv.dialect, inv.scenario, inv.client
    );
    DriverCommand {
        program: cargo(),
        args,
        describe,
    }
}

/// The per-scenario knobs the `capture` (claude) example needs, baked into the
/// dispatch table so the matrix is the single source of truth for *how* each
/// claude-code cell is recorded. Prompts and markers mirror the operator
/// commands documented in `docs/how-to/refresh-fixtures.md`.
struct ClaudePlan {
    /// A unique marker placed in the prompt; the harness filters captured
    /// exchanges to those whose body contains it (drops housekeeping calls).
    marker: &'static str,
    /// Which matching exchange to commit (0 = first; the tool-result→final turn
    /// is a later POST).
    capture_index: usize,
    /// The `--model` passed to `claude`.
    model: &'static str,
    /// The `--allowed-tools` value, when the scenario drives a tool.
    allowed_tools: Option<&'static str>,
    /// The prompt fed to `claude -p` on stdin.
    prompt: &'static str,
}

impl ClaudePlan {
    fn for_scenario(scenario: &str) -> ClaudePlan {
        let model = "claude-sonnet-4-5";
        match scenario {
            "single-text" => ClaudePlan {
                marker: "JIGTEXT10",
                capture_index: 0,
                model,
                allowed_tools: None,
                prompt: "JIGTEXT10 Reply with exactly: hello. Do not use any tools.",
            },
            "tool-call" => ClaudePlan {
                marker: "JIGCALL20",
                capture_index: 0,
                model,
                allowed_tools: Some("Write"),
                prompt: "JIGCALL20 Create a file named greeting.txt containing exactly: bar",
            },
            "tool-result-final" => ClaudePlan {
                marker: "JIGTOOL42",
                // The second routable POST: the tool-result→final turn.
                capture_index: 1,
                model,
                allowed_tools: Some("Write"),
                prompt: "JIGTOOL42 Create a file named greeting.txt containing exactly: bar",
            },
            "thinking-text" => ClaudePlan {
                marker: "JIGTHINK30",
                capture_index: 0,
                model,
                allowed_tools: None,
                prompt: "JIGTHINK30 Think step by step about what 17 times 23 is, \
                         then reply with just the number. Do not use any tools.",
            },
            "parallel-tool-calls" => ClaudePlan {
                marker: "JIGPAR91",
                capture_index: 0,
                model,
                allowed_tools: Some("Bash"),
                prompt: "JIGPAR91 Run two shell commands in parallel using two tool calls \
                         in the same turn: first 'echo hello' and second 'echo world'. \
                         Issue both Bash tool calls together in one turn.",
            },
            // An unknown scenario still produces a runnable command (the harness
            // will surface "no matching exchange" rather than recording garbage).
            _ => ClaudePlan {
                marker: "JIGUNKNOWN",
                capture_index: 0,
                model,
                allowed_tools: None,
                prompt: "JIGUNKNOWN unknown scenario; provide a prompt for this cell",
            },
        }
    }
}

/// The per-scenario knobs the `codex_capture` example needs. Prompts/markers
/// mirror the documented operator commands; `thinking-text` needs reasoning
/// enabled via `-c` config overrides (off by default in `codex exec`).
struct CodexPlan {
    marker: &'static str,
    capture_index: usize,
    /// Extra `-c key=value` overrides passed to `codex exec`.
    config: &'static [&'static str],
    prompt: &'static str,
}

impl CodexPlan {
    fn for_scenario(scenario: &str) -> CodexPlan {
        match scenario {
            "single-text" => CodexPlan {
                marker: "JIGTEXT11",
                capture_index: 0,
                config: &[],
                prompt: "JIGTEXT11 Reply with exactly: hello. Do not use any tools.",
            },
            "tool-call" => CodexPlan {
                marker: "JIGCALL21",
                capture_index: 0,
                config: &[],
                prompt: "JIGCALL21 Run the shell command: echo hello-from-codex using the \
                         exec_command tool.",
            },
            "tool-result-final" => CodexPlan {
                marker: "JIGEXEC55",
                capture_index: 1,
                config: &[],
                prompt: "JIGEXEC55 Run the shell command: echo hello-from-codex using the \
                         exec_command tool, then tell me what it printed.",
            },
            "thinking-text" => CodexPlan {
                marker: "JIGTHINK77",
                capture_index: 0,
                config: &[
                    "model_reasoning_effort=high",
                    "model_reasoning_summary=detailed",
                ],
                prompt: "JIGTHINK77 Think step by step about what 17 times 23 is, then reply \
                         with just the number. Do not use any tools.",
            },
            _ => CodexPlan {
                marker: "JIGUNKNOWN",
                capture_index: 0,
                config: &[],
                prompt: "JIGUNKNOWN unknown scenario; provide a prompt for this cell",
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provenance() -> Provenance {
        Provenance {
            captured: "2026-06-07".to_string(),
            recorder_sha: "deadbee".to_string(),
            upstream_host: None,
        }
    }

    fn inv(dialect: &str, scenario: &str, client: &str, role: Role) -> RecordInvocation {
        RecordInvocation {
            dialect: dialect.to_string(),
            scenario: scenario.to_string(),
            client: client.to_string(),
            role,
        }
    }

    /// Position of `flag`'s value in an argv, for spot-checking flag/value pairs.
    fn value_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .map(String::as_str)
    }

    #[test]
    fn openai_driver_uses_the_self_contained_example_and_threads_upstream() {
        let provenance = Provenance {
            upstream_host: Some("api.deepseek.com".to_string()),
            ..provenance()
        };
        let cmd = driver_for(
            &inv("openai", "single-text", "openai-sdk", Role::Authoritative),
            "fixtures",
            &provenance,
        );
        assert!(cmd.args.contains(&"jig-record".to_string()));
        assert_eq!(value_after(&cmd.args, "--example"), Some("openai_capture"));
        assert_eq!(value_after(&cmd.args, "--scenario"), Some("single-text"));
        assert_eq!(
            value_after(&cmd.args, "--upstream-host"),
            Some("api.deepseek.com")
        );
        assert_eq!(value_after(&cmd.args, "--captured"), Some("2026-06-07"));
        assert!(cmd.describe.contains("api.deepseek.com"));
    }

    #[test]
    fn openai_driver_omits_upstream_host_when_unset() {
        let cmd = driver_for(
            &inv("openai", "tool-call", "openai-sdk", Role::Authoritative),
            "fixtures",
            &provenance(),
        );
        assert!(!cmd.args.iter().any(|a| a == "--upstream-host"));
    }

    #[test]
    fn claude_driver_bakes_in_scenario_prompt_and_tools() {
        let cmd = driver_for(
            &inv(
                "anthropic",
                "tool-result-final",
                "claude-code",
                Role::Authoritative,
            ),
            "fixtures",
            &provenance(),
        );
        assert_eq!(value_after(&cmd.args, "--example"), Some("capture"));
        // tool-result-final commits the *second* matching POST.
        assert_eq!(value_after(&cmd.args, "--capture-index"), Some("1"));
        assert_eq!(value_after(&cmd.args, "--allowed-tools"), Some("Write"));
        assert_eq!(value_after(&cmd.args, "--match-body"), Some("JIGTOOL42"));
        // The prompt is the final positional, after the `--` separator.
        let sep = cmd.args.iter().rposition(|a| a == "--").unwrap();
        assert!(cmd.args[sep + 1].contains("JIGTOOL42"));
    }

    #[test]
    fn claude_single_text_drives_no_tools() {
        let cmd = driver_for(
            &inv(
                "anthropic",
                "single-text",
                "claude-code",
                Role::Authoritative,
            ),
            "fixtures",
            &provenance(),
        );
        assert!(!cmd.args.iter().any(|a| a == "--allowed-tools"));
        assert_eq!(value_after(&cmd.args, "--capture-index"), Some("0"));
    }

    #[test]
    fn codex_thinking_text_enables_reasoning_config() {
        let cmd = driver_for(
            &inv("codex", "thinking-text", "codex", Role::Authoritative),
            "fixtures",
            &provenance(),
        );
        assert_eq!(value_after(&cmd.args, "--example"), Some("codex_capture"));
        // Both reasoning config overrides are present.
        let configs: Vec<&String> = cmd
            .args
            .iter()
            .enumerate()
            .filter(|(i, _)| {
                i.checked_sub(1)
                    .map(|j| cmd.args[j] == "--config")
                    .unwrap_or(false)
            })
            .map(|(_, v)| v)
            .collect();
        assert!(
            configs
                .iter()
                .any(|c| c.contains("model_reasoning_effort=high"))
        );
        assert!(
            configs
                .iter()
                .any(|c| c.contains("model_reasoning_summary=detailed"))
        );
    }

    #[test]
    fn codex_tool_result_final_takes_second_post() {
        let cmd = driver_for(
            &inv("codex", "tool-result-final", "codex", Role::Authoritative),
            "fixtures",
            &provenance(),
        );
        assert_eq!(value_after(&cmd.args, "--capture-index"), Some("1"));
        assert_eq!(value_after(&cmd.args, "--match-body"), Some("JIGEXEC55"));
    }

    #[test]
    fn pi_subject_driver_selects_cell_by_dialect_and_scenario() {
        let cmd = driver_for(
            &inv("anthropic", "tool-call", "pi-sdk", Role::Subject),
            "fixtures",
            &provenance(),
        );
        assert!(cmd.args.contains(&"jig-oracle".to_string()));
        assert_eq!(
            value_after(&cmd.args, "--example"),
            Some("pi_subject_record")
        );
        assert_eq!(value_after(&cmd.args, "--dialect"), Some("anthropic"));
        assert_eq!(value_after(&cmd.args, "--scenario"), Some("tool-call"));
    }

    #[test]
    fn unknown_client_falls_back_to_generic_jig_record() {
        let cmd = driver_for(
            &inv(
                "openai",
                "single-text",
                "mystery-client",
                Role::Authoritative,
            ),
            "fixtures",
            &provenance(),
        );
        // Generic path spawns the `jig` bin, not an example.
        assert!(cmd.args.contains(&"--bin".to_string()));
        assert!(cmd.args.contains(&"jig".to_string()));
        assert!(!cmd.args.iter().any(|a| a == "--example"));
        assert!(cmd.describe.contains("manual"));
    }

    #[test]
    fn every_matrix_cell_resolves_to_a_runnable_driver() {
        // Each cell in the full matrix must produce a non-empty command whose
        // first arg is `run` (a cargo invocation).
        for inv in crate::matrix::plan(&crate::matrix::Selection::default()) {
            let cmd = driver_for(&inv, "fixtures", &provenance());
            assert_eq!(cmd.program, cargo());
            assert_eq!(cmd.args.first().map(String::as_str), Some("run"));
            assert!(!cmd.describe.is_empty());
        }
    }
}
