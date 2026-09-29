//! The recorder core's embedder: where captures leave the loop thread and log
//! lines reach the operator, since neither may happen in pure code.
//!
//! Shell code, run on the loop thread. Captures go over a channel, not into a
//! shared list: the handle's reader blocks on it, with a timeout, until the
//! capture it waits for arrives, and the channel closing says the loop is over.

use std::sync::mpsc::Sender;

use steploop::run::Host;
use steploop::time::Time;

use crate::relay::{Comp, Exchange, HostReq, RecorderCore};

/// Sends captures to a channel and prints log lines to stderr.
#[derive(Debug)]
pub struct RecorderHost {
    captures: Sender<Exchange>,
}

impl RecorderHost {
    pub fn new(captures: Sender<Exchange>) -> Self {
        RecorderHost { captures }
    }
}

impl Host<RecorderCore> for RecorderHost {
    fn handle(&mut self, _now: Time, reqs: &mut Vec<HostReq>, _comps: &mut Vec<Comp>) {
        for req in reqs.drain(..) {
            match req {
                // Nobody left to read it means nobody wants it.
                HostReq::Captured(exchange) => {
                    let _ = self.captures.send(exchange);
                }
                HostReq::Log(line) => eprintln!("{line}"),
            }
        }
    }
}
