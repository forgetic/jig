//! The `jig` binary — thin glue only.
//!
//! Start the service API with a built-in default `Script::Fixed`, print the
//! bound `base_url`, then block. Script-file loading is M6; this stays minimal
//! so the workspace builds and runs.

use std::io::{self, Read};

use jig_core::{Reply, Script};
use jig_server::FakeLlm;

fn main() -> io::Result<()> {
    // A trivial built-in script: one text reply for every request.
    let script = Script::Fixed(Reply::text("jig: fake LLM provider (M1 default script)."));

    let fake = FakeLlm::start(script)?;
    println!("{}", fake.base_url());

    // Block until stdin closes (e.g. Ctrl-D) or the process is signalled. The
    // FakeLlm is torn down by Drop when `fake` falls out of scope.
    let mut sink = Vec::new();
    let _ = io::stdin().read_to_end(&mut sink);

    Ok(())
}
