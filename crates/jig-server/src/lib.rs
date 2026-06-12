//! The embeddable `jig` service API.
//!
//! [`FakeLlm`] spawns one dedicated OS thread that hosts a **single-threaded**
//! skein runtime and serves a [`Script`] over HTTP until the handle is
//! dropped. Because the runtime lives on its own thread, callers never share
//! its executor: a *synchronous* test can [`FakeLlm::start`], make blocking
//! HTTP calls against [`FakeLlm::base_url`], and let [`Drop`] tear the thread
//! down — no async runtime of its own (see bootstrap.md "Public API").

use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use jig_core::{RecordedRequest, Script};
use skein::sync::Notify;

mod server;

use server::RequestLog;

/// A running fake LLM provider.
///
/// Holds the runtime thread's join handle, the bound address (for
/// [`base_url`](FakeLlm::base_url)), and the shutdown signal. Dropping it
/// signals shutdown and joins the thread.
pub struct FakeLlm {
    addr: SocketAddr,
    /// Shared with the serve loop; `notify_one` is synchronous and stores the
    /// notification if the loop is not waiting yet, so a `Drop` racing the
    /// accept loop never loses the signal.
    shutdown: Arc<Notify>,
    thread: Option<JoinHandle<()>>,
    /// Shared with the runtime thread, which appends one entry per request.
    log: RequestLog,
}

impl FakeLlm {
    /// Spawn a dedicated OS thread hosting a single-threaded skein runtime
    /// that serves `script` until this handle is dropped.
    ///
    /// The listener is bound on the runtime thread *before* this returns, so
    /// [`base_url`](FakeLlm::base_url) is valid the instant `start` yields.
    pub fn start(script: Script) -> io::Result<FakeLlm> {
        let shutdown = Arc::new(Notify::new());
        let server_shutdown = Arc::clone(&shutdown);
        // The runtime thread sends back the bound address (or a bind error).
        let (addr_tx, addr_rx) = std::sync::mpsc::channel::<io::Result<SocketAddr>>();
        let script = Arc::new(script);

        // The request log is shared with the runtime thread (which appends) and
        // kept on the handle (read by `requests()`).
        let log: RequestLog = Arc::new(Mutex::new(Vec::new()));
        let server_log = Arc::clone(&log);

        let thread = std::thread::Builder::new()
            .name("jig-runtime".to_string())
            .spawn(move || {
                // Sockets need a task's capability context, so the whole serve
                // future runs as a spawned task with its `Cx` handed in
                // explicitly (skein has no ambient context).
                jig_runtime::block_on(move |cx| async move {
                    // Bind first; report the result back to `start`.
                    let listener = match skein::net::TcpListener::bind(("127.0.0.1", 0)).await {
                        Ok(listener) => listener,
                        Err(err) => {
                            let _ = addr_tx.send(Err(err));
                            return;
                        }
                    };
                    match listener.local_addr() {
                        Ok(addr) => {
                            if addr_tx.send(Ok(addr)).is_err() {
                                // Caller gave up before we bound; nothing to serve.
                                return;
                            }
                        }
                        Err(err) => {
                            let _ = addr_tx.send(Err(err));
                            return;
                        }
                    }

                    server::serve(&cx, listener, script, server_log, server_shutdown).await;
                });
            })?;

        // Wait for the bind result. If the thread died before sending, surface
        // a clean error instead of hanging.
        let addr = match addr_rx.recv() {
            Ok(result) => result?,
            Err(_) => {
                let _ = thread.join();
                return Err(io::Error::other("jig runtime thread exited before binding"));
            }
        };

        Ok(FakeLlm {
            addr,
            shutdown,
            thread: Some(thread),
            log,
        })
    }

    /// The base URL clients should target, e.g. `"http://127.0.0.1:54321"`.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// A snapshot of every request the server has handled, in arrival order.
    ///
    /// Returns a clone so the caller can assert at leisure without holding the
    /// lock. Safe to call from the caller's thread while the runtime thread
    /// keeps serving — the log is shared behind a `Mutex`.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.log.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

impl Drop for FakeLlm {
    fn drop(&mut self) {
        // Signal shutdown; the accept loop selects on this and returns.
        self.shutdown.notify_one();
        // Join the runtime thread so the port is released by the time `drop`
        // returns.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
