//! # met-ipc
//!
//! Phase 3 of the strangler rewrite: the Unix-socket contract that lets the
//! Python backend's `Machine` become a thin client of `met-daemon` behind
//! the `use_rust_serial` config flag.
//!
//! Wire format: newline-delimited JSON, versioned hello, fire-and-forget
//! commands, completion observed via the event stream (see
//! [`protocol`] docs). The Python counterpart lives in
//! `esp_serial/rust_daemon_client.py`.

pub mod protocol;
pub mod server;

pub use protocol::{ClientCommand, ServerFrame, PROTOCOL_VERSION};
pub use server::{serve, socket_path_from_env};
