//! Shared skein runtime bootstrap for jig's crates.
//!
//! skein exposes no ambient context: sockets and timers only work inside a
//! spawned *task*, and the task's capability context (`Cx`) is handed to the
//! task body factory-style — a bare `Runtime::block_on` future has neither.
//! [`block_on`] therefore builds a single-threaded runtime with the I/O
//! reactor attached, spawns the caller's future via `spawn_with_cx` (so the
//! body receives its `Cx` explicitly and can pass it down), and parks the
//! runtime on a tiny mutex+waker slot until the task delivers its result.
//!
//! The shape is adapted from tongs' `runtime.rs` (the engine-runtime pattern
//! shared by our skein consumers), with the `Cx` made explicit end to end.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};

use skein::cx::Cx;
use skein::runtime::reactor::create_reactor;
use skein::runtime::{Runtime, RuntimeBuilder};

pub mod io;

pub use io::read_some;

/// Build the runtime jig uses everywhere: single-threaded, reactor attached,
/// and a small blocking pool (DNS resolution and filesystem helpers run there).
pub fn build_runtime() -> Result<Runtime, String> {
    let reactor =
        create_reactor().map_err(|error| format!("creating skein reactor failed: {error}"))?;
    RuntimeBuilder::current_thread()
        .blocking_threads(1, 4)
        .with_reactor(reactor)
        .build()
        .map_err(|error| format!("building skein runtime failed: {error}"))
}

/// Build a runtime and run one future to completion **as a task**, handing the
/// body its [`Cx`]. Panics from the future are propagated to the caller.
pub fn block_on<F, Fut>(f: F) -> Fut::Output
where
    F: FnOnce(Cx) -> Fut + Send + 'static,
    Fut: Future + Send + 'static,
    Fut::Output: Send + 'static,
{
    let runtime = build_runtime().expect("build skein runtime");
    block_on_runtime(&runtime, f)
}

/// [`block_on`] on an already-built runtime.
pub fn block_on_runtime<F, Fut>(runtime: &Runtime, f: F) -> Fut::Output
where
    F: FnOnce(Cx) -> Fut + Send + 'static,
    Fut: Future + Send + 'static,
    Fut::Output: Send + 'static,
{
    let (sender, receiver) = result_slot();
    runtime.handle().spawn_with_cx(move |cx| async move {
        let mut future = Box::pin(f(cx));
        let outcome = std::future::poll_fn(move |task_cx| {
            let poll = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                future.as_mut().poll(task_cx)
            }));
            match poll {
                Ok(Poll::Ready(value)) => Poll::Ready(Ok(value)),
                Ok(Poll::Pending) => Poll::Pending,
                Err(payload) => Poll::Ready(Err(payload)),
            }
        })
        .await;
        sender.send(outcome);
    });
    match runtime.block_on(receiver.recv()) {
        Some(Ok(value)) => value,
        Some(Err(payload)) => std::panic::resume_unwind(payload),
        None => panic!("jig task vanished without a result"),
    }
}

/// A minimal runtime-agnostic oneshot (plain mutex + waker), so the result can
/// be awaited from the raw `Runtime::block_on` context which has no `Cx`.
fn result_slot<T>() -> (SlotSender<T>, SlotReceiver<T>) {
    let shared = Arc::new(Mutex::new(Slot {
        value: None,
        waker: None,
        closed: false,
    }));
    (
        SlotSender {
            shared: Arc::clone(&shared),
        },
        SlotReceiver { shared },
    )
}

struct Slot<T> {
    value: Option<T>,
    waker: Option<Waker>,
    closed: bool,
}

struct SlotSender<T> {
    shared: Arc<Mutex<Slot<T>>>,
}

impl<T> SlotSender<T> {
    fn send(self, value: T) {
        let mut slot = self.shared.lock().expect("slot lock");
        slot.value = Some(value);
        if let Some(waker) = slot.waker.take() {
            waker.wake();
        }
    }
}

impl<T> Drop for SlotSender<T> {
    fn drop(&mut self) {
        let mut slot = self.shared.lock().expect("slot lock");
        slot.closed = true;
        if let Some(waker) = slot.waker.take() {
            waker.wake();
        }
    }
}

struct SlotReceiver<T> {
    shared: Arc<Mutex<Slot<T>>>,
}

impl<T> SlotReceiver<T> {
    async fn recv(self) -> Option<T> {
        std::future::poll_fn(move |task_cx| {
            let mut slot = self.shared.lock().expect("slot lock");
            if let Some(value) = slot.value.take() {
                return Poll::Ready(Some(value));
            }
            if slot.closed {
                return Poll::Ready(None);
            }
            slot.waker = Some(task_cx.waker().clone());
            Poll::Pending
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn block_on_returns_value() {
        assert_eq!(super::block_on(|_cx| async { 41 + 1 }), 42);
    }

    #[test]
    #[should_panic(expected = "boom")]
    fn block_on_propagates_panics() {
        super::block_on(|_cx| async { panic!("boom") });
    }
}
