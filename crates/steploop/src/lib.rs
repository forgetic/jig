//! A no-await, single-threaded event loop for sans-IO step functions.
//!
//! One loop per thread; its only blocking call is [`reactor::Reactor::poll`].
//! Business logic ([`run::Core`]) and I/O planning ([`run::IoStep`]) are pure
//! step functions; [`reactor::Reactor::perform`] executes the planned
//! non-blocking syscalls ([`sys::Action`]) and reports one [`sys::Event`] per
//! action. See `docs/explanation/sans-io-shell.md` for the design and the
//! reasoning behind it.
//!
//! This crate knows nothing about jig: it is meant to be adopted by temper
//! and eventually to move into skein.

pub mod http1;
pub mod reactor;
pub mod run;
pub mod sys;
pub mod tcp;
pub mod time;
#[cfg(feature = "tls")]
pub mod tls;
