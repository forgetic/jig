//! The embeddable `jig` service API.
//!
//! jig is split so that each user takes only the layers it needs (design
//! `docs/explanation/sans-io-shell.md` §5.1 and §5.6):
//!
//! - [`provider`], the pure core: HTTP request messages in; responses, request
//!   records and rule decisions out. [`serve_request`] drives it in process,
//!   with no I/O at all (§8).
//! - [`ServerIo`], its I/O step: steploop's HTTP/1 server planner, translated
//!   to the core's vocabulary. Pure as well.
//! - [`FakeLlmHost`], its embedder: it keeps the [`RequestLog`] and answers
//!   rule decisions with the script's closure.
//! - [`FakeLlm`], the three on one OS thread, driven by [`steploop::run::run`]
//!   over a loopback listener.
//!
//! Each is public, so an embedder can build its own loop from the parts
//! [`FakeLlm`] uses.

use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, PoisonError};
use std::thread::JoinHandle;

use jig_core::{RecordedRequest, Script};
use steploop::http1::server::{Config, Server};
use steploop::reactor::{Reactor, SignalSender};
use steploop::run::{NoTap, run};
use steploop::sys::Ids;

pub mod host;
pub mod io;
pub mod provider;

pub use host::{FakeLlmHost, RequestLog};
pub use io::ServerIo;
pub use provider::{Provider, ProviderConfig, serve_request};

/// A running fake LLM provider.
///
/// Its loop runs on a thread of its own, so a *synchronous* test can
/// [`start`](FakeLlm::start) one, make blocking HTTP calls against
/// [`base_url`](FakeLlm::base_url), and let [`Drop`] stop it, with no runtime
/// of its own. Connections are served concurrently: an idle or slow client
/// doesn't hold up the others.
pub struct FakeLlm {
    addr: SocketAddr,
    /// Raised by `Drop`; the loop turns it into [`provider::Comp::Stop`].
    stop: SignalSender,
    thread: Option<JoinHandle<()>>,
    /// Appended to by the loop thread's host.
    log: RequestLog,
}

impl FakeLlm {
    /// Serve `script` on a loopback port until this handle is dropped.
    ///
    /// Everything that can fail is set up here, on the caller's thread, before
    /// the loop thread starts, so errors come back from `start` and
    /// [`base_url`](FakeLlm::base_url) is valid as soon as it returns: the
    /// listener is bound, and connections wait in its backlog until the loop
    /// accepts them.
    pub fn start(script: Script) -> std::io::Result<FakeLlm> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let addr = listener.local_addr()?;
        let (plan, rule) = script.split();

        let server = Server::new(Ids::new(), Config::default());
        let mut reactor = Reactor::new()?;
        reactor.adopt_listener(server.listener(), listener)?;
        let (stop_id, stop) = reactor.signal()?;

        let log = RequestLog::default();
        let mut host = FakeLlmHost::new(Arc::clone(&log), rule);
        let mut io = ServerIo::new(server, stop_id);
        let mut provider = Provider::new(plan);
        let thread = std::thread::Builder::new()
            .name("jig-fakellm".to_string())
            .spawn(move || {
                // Only `Drop` waits for this thread, and it can't return an
                // error, so stderr is the one place left to report it.
                if let Err(err) = run(&mut provider, &mut io, &mut host, &mut reactor, &mut NoTap) {
                    eprintln!("jig: the FakeLlm loop failed: {err}");
                }
            })?;

        Ok(FakeLlm {
            addr,
            stop,
            thread: Some(thread),
            log,
        })
    }

    /// The base URL clients should target, e.g. `"http://127.0.0.1:54321"`.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// A snapshot of every request handled so far, in the order the script
    /// saw them (with concurrent clients, the order their requests completed).
    ///
    /// Returns a clone, so the caller can assert at leisure while the loop
    /// keeps serving.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for FakeLlm {
    /// Stop the loop and wait for it, so the port is released on return.
    /// Responses still being written get the shutdown grace
    /// ([`ProviderConfig::grace`], 1 s) and are then cut, so a client that
    /// stops reading can't hold this up for longer.
    fn drop(&mut self) {
        // A failed raise means the loop has already ended.
        let _ = self.stop.raise();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
