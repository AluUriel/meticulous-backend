//! # met-daemon
//!
//! Phase 2 of the strangler rewrite: the serial daemon that owns the ESP32
//! UART and GPIO. It runs the read loop as a pure state machine
//! ([`reader::ReaderState`]) driven by a transport (real serial or the
//! fixture-replay emulator), publishes typed [`events::MachineEvent`]s and
//! [`reader::MachineSnapshot`]s, and encodes outgoing actions/profiles.
//!
//! What deliberately stays in the Python backend (until phase 3+): sounds,
//! shot/DB management, notifications, alarms UI, firmware flashing (esptool),
//! config sync, and every REST/socket.io surface. The daemon reports; the
//! backend decides.

pub mod command;
pub mod emulator;
pub mod events;
pub mod gpio;
pub mod reader;
pub mod runtime;
pub mod transport;

pub use command::EspAction;
pub use events::{Alarm, MachineEvent};
pub use reader::{MachineSnapshot, ReaderState};
pub use runtime::{run, DaemonCommand, DaemonHandle, DaemonOptions};
pub use transport::Transport;
