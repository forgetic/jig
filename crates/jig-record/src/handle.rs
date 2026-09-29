//! [`Recorder`]: the core, its I/O step and its host on one OS thread, driven
//! by `steploop::run::run` over a loopback listener, as `FakeLlm` is.
//!
//! Connections are served concurrently, so a client's idle pooled
//! connections or its preflights never hold up the request that matters.

use std::io;
use std::net::{SocketAddr, TcpListener};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;

use rustls::RootCertStore;
use steploop::reactor::{Reactor, SignalSender};
use steploop::run::{NoTap, run};
use steploop::tls::client_config;

use crate::host::RecorderHost;
use crate::io::RecorderIo;
use crate::relay::{Exchange, Mode, RecorderCore, UpstreamOverride};

/// What [`Recorder::start`] sets up.
#[derive(Clone, Debug, Default)]
pub struct RecorderConfig {
    pub mode: Mode,
    /// Forward to this host instead of the dialect's own (DeepSeek or a
    /// gateway for the OpenAI dialect, say). It is also the `Host` header and
    /// the TLS server name.
    pub upstream_host: Option<String>,
    /// Connect here instead of resolving the host (a test hook).
    pub upstream: Option<UpstreamOverride>,
    /// Trust these roots instead of webpki's (a test hook).
    pub roots: Option<RootCertStore>,
}

/// A running recorder. See the module docs.
pub struct Recorder {
    addr: SocketAddr,
    /// Raised by [`Recorder::stop`] and `Drop`; the loop turns it into
    /// [`crate::relay::Comp::Stop`].
    stop: SignalSender,
    thread: Option<JoinHandle<()>>,
    captures: Receiver<Exchange>,
}

impl Recorder {
    /// Listen on a loopback port and start recording.
    ///
    /// Everything that can fail is set up here, on the caller's thread, before
    /// the loop thread starts, so [`base_url`](Recorder::base_url) is valid
    /// as soon as this returns.
    pub fn start(config: RecorderConfig) -> io::Result<Recorder> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let addr = listener.local_addr()?;
        let tls = client_config(config.roots).map_err(io::Error::other)?;
        let mut reactor = Reactor::new()?;
        let (stop_id, stop) = reactor.signal()?;
        let mut io = RecorderIo::new(tls, stop_id);
        reactor.adopt_listener(io.listener(), listener)?;

        let (tx, captures) = mpsc::channel();
        let mut host = RecorderHost::new(tx);
        let mut core = RecorderCore::new(config.mode, config.upstream_host, config.upstream);
        let thread = std::thread::Builder::new()
            .name("jig-recorder".to_string())
            .spawn(move || {
                // Nothing waits on this thread's result, so stderr is the one
                // place left to report it.
                if let Err(err) = run(&mut core, &mut io, &mut host, &mut reactor, &mut NoTap) {
                    eprintln!("jig: the recorder loop failed: {err}");
                }
            })?;

        Ok(Recorder {
            addr,
            stop,
            thread: Some(thread),
            captures,
        })
    }

    /// The loopback base URL the client should be pointed at.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Wait up to `timeout` for the next capture. Fails with `TimedOut` if
    /// none came, and with another error once the recorder has stopped and
    /// every capture has been taken: in once mode, that means the exchange
    /// failed, and the log on stderr says why.
    pub fn next_capture(&self, timeout: Duration) -> io::Result<Exchange> {
        self.captures.recv_timeout(timeout).map_err(|e| match e {
            RecvTimeoutError::Timeout => io::Error::new(
                io::ErrorKind::TimedOut,
                format!("the recorder captured nothing within {timeout:?}"),
            ),
            RecvTimeoutError::Disconnected => {
                io::Error::other("the recorder stopped without capturing an exchange")
            }
        })
    }

    /// Stop, and return the captures [`next_capture`](Recorder::next_capture)
    /// hasn't taken, in the order they completed. Relays in flight get
    /// [`crate::relay::GRACE`] to finish, and are captured if they do.
    pub fn stop(mut self) -> Vec<Exchange> {
        self.join();
        self.captures.try_iter().collect()
    }

    fn join(&mut self) {
        if let Some(thread) = self.thread.take() {
            // A failed raise means the loop has already ended.
            let _ = self.stop.raise();
            let _ = thread.join();
        }
    }
}

impl Drop for Recorder {
    /// Stop and wait for the loop, as [`Recorder::stop`] does.
    fn drop(&mut self) {
        self.join();
    }
}
