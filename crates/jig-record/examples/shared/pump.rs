//! The concurrent capture pump shared by the manual example harnesses.
//!
//! Real official clients pre-open a *pool* of connections and send the request
//! that matters on one of them, so the recorder must accept connections
//! **concurrently** — one task per connection — rather than serially. A serial
//! accept loop blocks reading an idle pooled socket and never reaches the one
//! carrying the `POST`.
//!
//! [`CapturePump`] runs that concurrent accept loop on a dedicated OS thread
//! hosting a single-threaded skein runtime (sockets only work inside a spawned
//! task, whose `Cx` skein hands over explicitly), while the example's main
//! thread drives the official client synchronously. `stop` signals the loop,
//! joins the thread, and returns every captured routable exchange.

use std::io;
use std::net::SocketAddr;
use std::pin::pin;
use std::sync::{Arc, Mutex};

use jig_record::proxy::{bind, handle_connection};
use jig_record::{ClientRequest, Route, UpstreamResponse};
use skein::combinator::{Either, Select};
use skein::sync::Notify;

/// One captured routable exchange: the client request, the upstream response,
/// and the route it was forwarded on.
pub type Exchange = (ClientRequest, UpstreamResponse, Route);

/// A passthrough recorder accepting connections concurrently on its own
/// runtime thread.
pub struct CapturePump {
    addr: SocketAddr,
    captured: Arc<Mutex<Vec<Exchange>>>,
    stop: Arc<Notify>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl CapturePump {
    /// Bind the recorder on an ephemeral loopback port and start accepting.
    ///
    /// `upstream_host_override` is threaded through to
    /// [`handle_connection`] (DeepSeek-style overrides on the openai dialect).
    pub fn start(upstream_host_override: Option<String>) -> io::Result<CapturePump> {
        let captured: Arc<Mutex<Vec<Exchange>>> = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(Notify::new());
        let (addr_tx, addr_rx) = std::sync::mpsc::channel::<io::Result<SocketAddr>>();

        let pump_captured = Arc::clone(&captured);
        let pump_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("jig-capture-pump".to_string())
            .spawn(move || {
                let runtime = match jig_runtime::build_runtime() {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        let _ = addr_tx.send(Err(io::Error::other(err)));
                        return;
                    }
                };
                let handle = runtime.handle();
                jig_runtime::block_on_runtime(&runtime, move |cx| async move {
                    let listener = match bind(&cx).await {
                        Ok(listener) => listener,
                        Err(err) => {
                            let _ = addr_tx.send(Err(err));
                            return;
                        }
                    };
                    match listener.local_addr() {
                        Ok(addr) => {
                            if addr_tx.send(Ok(addr)).is_err() {
                                return;
                            }
                        }
                        Err(err) => {
                            let _ = addr_tx.send(Err(err));
                            return;
                        }
                    }

                    loop {
                        let notified = pin!(pump_stop.notified());
                        let accepted = pin!(listener.accept());
                        match Select::new(notified, accepted).await {
                            Either::Left(()) => return,
                            Either::Right(Ok((client, _peer))) => {
                                let captured_conn = Arc::clone(&pump_captured);
                                let upstream = upstream_host_override.clone();
                                // One task per connection, each with its own Cx.
                                handle.spawn_with_cx(move |cx| async move {
                                    match handle_connection(&cx, client, upstream.as_deref())
                                        .await
                                    {
                                        Ok(Some(triple)) => {
                                            let mut guard = captured_conn
                                                .lock()
                                                .unwrap_or_else(|p| p.into_inner());
                                            let n = guard.len();
                                            let (req, resp, _) = &triple;
                                            eprintln!(
                                                "captured exchange #{n} {} {} -> {} ({} body bytes)",
                                                req.method,
                                                req.path(),
                                                resp.status,
                                                resp.body.len()
                                            );
                                            guard.push(triple);
                                        }
                                        Ok(None) => {} // preflight, answered with 204
                                        Err(e) => eprintln!("connection error: {e}"),
                                    }
                                });
                            }
                            Either::Right(Err(e)) => {
                                eprintln!("accept ended: {e}");
                                return;
                            }
                        }
                    }
                });
            })?;

        let addr = match addr_rx.recv() {
            Ok(result) => result?,
            Err(_) => {
                let _ = thread.join();
                return Err(io::Error::other(
                    "capture pump thread exited before binding",
                ));
            }
        };

        Ok(CapturePump {
            addr,
            captured,
            stop: Arc::clone(&stop),
            thread: Some(thread),
        })
    }

    /// The loopback base URL the client should be pointed at.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Stop accepting, join the pump thread, and return the captured
    /// exchanges in arrival order.
    pub fn stop(mut self) -> Vec<Exchange> {
        self.stop.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        std::mem::take(&mut *self.captured.lock().unwrap_or_else(|p| p.into_inner()))
    }
}
