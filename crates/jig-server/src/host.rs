//! The provider core's embedder: where the request log and the rule closure
//! live, since neither may enter pure code.
//!
//! This is shell code, run on the loop thread. It takes the design's one lock,
//! at the thread boundary: the log is shared with whoever reads it from
//! another thread, and a query answered by the loop would make that reader
//! wait on the loop for no benefit. It is public so that an embedder with a
//! loop of its own can reuse it. See `docs/explanation/sans-io-shell.md` §4.9
//! and §4.10.

use std::sync::{Arc, Mutex, PoisonError};

use jig_core::{HttpError, RecordedRequest, Rule, ScriptAction};
use steploop::run::Host;
use steploop::time::Time;

use crate::provider::{Comp, HostReq, Provider};

/// Every request the provider handled, in the order it handled them.
pub type RequestLog = Arc<Mutex<Vec<RecordedRequest>>>;

/// Appends records to a [`RequestLog`] and answers decisions with a [`Rule`].
#[derive(Debug)]
pub struct FakeLlmHost {
    log: RequestLog,
    /// The closure [`Script::split`](jig_core::Script::split) set aside, if
    /// the script was a rule.
    rule: Option<Rule>,
}

impl FakeLlmHost {
    pub fn new(log: RequestLog, rule: Option<Rule>) -> Self {
        FakeLlmHost { log, rule }
    }
}

impl Host<Provider> for FakeLlmHost {
    fn handle(&mut self, _now: Time, reqs: &mut Vec<HostReq>, comps: &mut Vec<Comp>) {
        for req in reqs.drain(..) {
            match req {
                // A reader that panicked holding the lock left the log intact.
                HostReq::Record(record) => self
                    .log
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(record),
                // Answered in the same round, so a decision is never pending
                // across iterations.
                HostReq::Decide { req, view } => {
                    let action = match &mut self.rule {
                        Some(rule) => rule.decide(&view),
                        // A plan that asks with no rule to answer is a wiring
                        // mistake; the client should see it, not a hang.
                        None => ScriptAction::HttpError(HttpError::provider(
                            500,
                            "no_rule",
                            "the script asked for a rule decision, but none was given",
                        )),
                    };
                    comps.push(Comp::Decision { req, action });
                }
            }
        }
    }
}
