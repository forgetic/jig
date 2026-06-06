//! The embeddable `jig` service API.
//!
//! [`FakeLlm`] spawns one dedicated OS thread that hosts a **single-threaded**
//! tokio runtime (`new_current_thread`) and serves a [`Script`] over HTTP until
//! the handle is dropped. Because the runtime lives on its own thread, callers
//! never share its executor: a *synchronous* test can [`FakeLlm::start`], make
//! blocking `reqwest` calls against [`FakeLlm::base_url`], and let [`Drop`] tear
//! the thread down — no async runtime of its own (see bootstrap.md "Public
//! API").

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::thread::JoinHandle;

use jig_core::Script;
use tokio::sync::oneshot;

mod server;

/// A running fake LLM provider.
///
/// Holds the runtime thread's join handle, the bound address (for
/// [`base_url`](FakeLlm::base_url)), and the shutdown signal. Dropping it
/// signals shutdown and joins the thread.
pub struct FakeLlm {
    addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl FakeLlm {
    /// Spawn a dedicated OS thread hosting a single-threaded tokio runtime that
    /// serves `script` until this handle is dropped.
    ///
    /// The listener is bound on the runtime thread *before* this returns, so
    /// [`base_url`](FakeLlm::base_url) is valid the instant `start` yields.
    pub fn start(script: Script) -> io::Result<FakeLlm> {
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        // The runtime thread sends back the bound address (or a bind error).
        let (addr_tx, addr_rx) = std::sync::mpsc::channel::<io::Result<SocketAddr>>();
        let script = Arc::new(script);

        let thread = std::thread::Builder::new()
            .name("jig-runtime".to_string())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(err) => {
                        let _ = addr_tx.send(Err(err));
                        return;
                    }
                };

                runtime.block_on(async move {
                    // Bind first; report the result back to `start`.
                    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
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

                    server::serve(listener, script, shutdown_rx).await;
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
            shutdown: Some(shutdown_tx),
            thread: Some(thread),
        })
    }

    /// The base URL clients should target, e.g. `"http://127.0.0.1:54321"`.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for FakeLlm {
    fn drop(&mut self) {
        // Signal shutdown; the accept loop selects on this and returns.
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        // Join the runtime thread so the port is released by the time `drop`
        // returns.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
