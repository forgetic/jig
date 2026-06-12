//! The `jig` binary — thin glue only.
//!
//! Two modes, both thin wiring over the `crates/`:
//!
//! - **serve** (default): load a [`Script`] from a script file (or fall back to a
//!   built-in default), start the service API with it, print the bound
//!   `base_url`, then block. This is the *same* [`FakeLlm::start`] call the
//!   in-process tests use; there is no second implementation.
//! - **record**: stand up the passthrough recorder ([`jig_record`]) in front of a
//!   real LLM backend, capture one client ↔ backend exchange to a redacted,
//!   client/role-tagged fixture, then exit. The binary's only job here is to
//!   gather provenance (capture date, recorder git sha, client label/role) and
//!   hand it to [`jig_record::record_once`]; all capture logic lives in the crate.
//!
//! # Usage
//!
//! ```sh
//! jig [SCRIPT_FILE]
//! jig record --client <label> --scenario <name> [options]
//! ```

use std::io::{self, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use jig_core::{Reply, Script, ScriptFile};
use jig_record::{Provenance, Role};
use jig_server::FakeLlm;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("jig: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    let first = args.next();

    match first.as_deref() {
        Some("record") => run_record(args.collect()),
        // Anything else is treated as the optional script-file path for serve
        // mode, preserving the original `jig [SCRIPT_FILE]` interface.
        other => run_serve(other.map(str::to_string)),
    }
}

/// Serve mode: load a script, start the fake LLM, print `base_url`, block.
fn run_serve(script_path: Option<String>) -> io::Result<()> {
    let script = load_script(script_path)?;

    let fake = FakeLlm::start(script)?;
    println!("{}", fake.base_url());

    // Block until stdin closes (e.g. Ctrl-D) or the process is signalled. The
    // FakeLlm is torn down by Drop when `fake` falls out of scope.
    let mut sink = Vec::new();
    let _ = io::stdin().read_to_end(&mut sink);

    Ok(())
}

/// Record mode: run one passthrough capture.
///
/// Stands up a single-threaded skein runtime (same shape as the server), prints
/// the loopback `base_url` for the official client to target, and writes one
/// redacted recording. Recording is manual — it needs a live API key on the
/// client side and network — so this path is never exercised by `cargo test`.
fn run_record(args: Vec<String>) -> io::Result<()> {
    let opts = RecordOpts::parse(args)?;

    let provenance = Provenance {
        client: opts.client,
        role: opts.role,
        scenario: opts.scenario,
        client_version: opts.client_version,
        captured: opts.captured,
        recorder_sha: opts.recorder_sha,
    };
    // The recorder owns the skein runtime so this binary stays runtime-free,
    // mirroring how `jig-server` hides its runtime behind `FakeLlm`.
    let path = jig_record::record_once_blocking(
        &opts.fixtures_root,
        &provenance,
        opts.upstream_host.as_deref(),
        io::stdout(),
    )?;

    eprintln!("jig: wrote recording to {}", path.display());
    Ok(())
}

/// Parsed `jig record` options.
struct RecordOpts {
    client: String,
    role: Role,
    scenario: String,
    client_version: Option<String>,
    captured: String,
    recorder_sha: String,
    fixtures_root: PathBuf,
    upstream_host: Option<String>,
}

impl RecordOpts {
    /// Parse `--flag value` pairs. `--client` and `--scenario` are required; the
    /// rest default sensibly (capture date and recorder sha are discovered from
    /// the environment / git, role defaults to `authoritative`).
    fn parse(args: Vec<String>) -> io::Result<RecordOpts> {
        let mut client = None;
        let mut role = Role::Authoritative;
        let mut scenario = None;
        let mut client_version = None;
        let mut captured = None;
        let mut recorder_sha = None;
        let mut fixtures_root = PathBuf::from("fixtures");
        let mut upstream_host = None;

        let mut it = args.into_iter();
        while let Some(flag) = it.next() {
            let mut value = || {
                it.next().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, format!("{flag} needs a value"))
                })
            };
            match flag.as_str() {
                "--client" => client = Some(value()?),
                "--scenario" => scenario = Some(value()?),
                "--client-version" => client_version = Some(value()?),
                "--captured" => captured = Some(value()?),
                "--recorder-sha" => recorder_sha = Some(value()?),
                "--fixtures-root" => fixtures_root = PathBuf::from(value()?),
                "--upstream-host" => upstream_host = Some(value()?),
                "--role" => {
                    role = match value()?.as_str() {
                        "authoritative" => Role::Authoritative,
                        "subject" => Role::Subject,
                        other => {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidInput,
                                format!("unknown role {other:?} (want authoritative|subject)"),
                            ));
                        }
                    }
                }
                other => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("unknown record flag {other:?}"),
                    ));
                }
            }
        }

        Ok(RecordOpts {
            client: required(client, "--client")?,
            role,
            scenario: required(scenario, "--scenario")?,
            client_version,
            captured: captured.unwrap_or_else(default_captured),
            recorder_sha: recorder_sha.unwrap_or_else(default_recorder_sha),
            fixtures_root,
            upstream_host,
        })
    }
}

/// Require a flag to have been provided.
fn required(value: Option<String>, flag: &str) -> io::Result<String> {
    value.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("{flag} is required")))
}

/// Capture date: `$JIG_CAPTURE_DATE` if set, else `unknown` (the binary does not
/// link a date crate; callers pass `--captured` or set the env for a real date).
fn default_captured() -> String {
    std::env::var("JIG_CAPTURE_DATE").unwrap_or_else(|_| "unknown".to_string())
}

/// Recorder git sha: `$JIG_RECORDER_SHA` if set, else `git rev-parse --short
/// HEAD`, else `unknown`. Discovering it here keeps the core clock/VCS-free.
fn default_recorder_sha() -> String {
    if let Ok(sha) = std::env::var("JIG_RECORDER_SHA") {
        return sha;
    }
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Resolve the [`Script`] to serve: load it from `path` if one was given, else
/// the built-in default. Loading a file is the only argument wiring this binary
/// does; the script format itself lives in [`jig_core::script_file`].
fn load_script(path: Option<String>) -> io::Result<Script> {
    match path {
        Some(path) => Ok(ScriptFile::load(&path)?.into_script()),
        None => Ok(default_script()),
    }
}

/// The built-in fallback: one fixed text reply for every request.
fn default_script() -> Script {
    Script::Fixed(Reply::text(
        "jig: fake LLM provider (built-in default script).",
    ))
}
