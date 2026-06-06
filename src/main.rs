//! The `jig` binary — thin glue only.
//!
//! Load a [`Script`] from a script file (or fall back to a built-in default),
//! start the service API with it, print the bound `base_url`, then block. This is
//! the *same* [`FakeLlm::start`] call the in-process tests use; there is no second
//! implementation and **no behaviour beyond argument/script wiring** (all of that
//! lives in the `crates/`).
//!
//! # Usage
//!
//! ```sh
//! jig [SCRIPT_FILE]
//! ```
//!
//! With no argument, `jig` serves a built-in default script (one fixed text reply
//! for every request). With a path, it loads that JSON script file — see
//! [`jig_core::script_file`] for the schema.

use std::io::{self, Read};
use std::process::ExitCode;

use jig_core::{Reply, Script, ScriptFile};
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
    let script = load_script(std::env::args().nth(1))?;

    let fake = FakeLlm::start(script)?;
    println!("{}", fake.base_url());

    // Block until stdin closes (e.g. Ctrl-D) or the process is signalled. The
    // FakeLlm is torn down by Drop when `fake` falls out of scope.
    let mut sink = Vec::new();
    let _ = io::stdin().read_to_end(&mut sink);

    Ok(())
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
