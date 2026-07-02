//! met-daemon binary: run the ESP32 serial daemon with its IPC server.
//!
//! Configuration comes from the same environment variables the Python
//! backend uses:
//! - `BACKEND`: `FIKA` (default on the machine), `USB`, or `EMULATOR`
//! - `EMULATION_SPEED`: percent, 100 = real time
//! - `METICULOUS_EMULATION_DIR`: directory with `emulated.*.json` fixtures
//! - `METICULOUS_IPC_SOCKET`: Unix socket path (default /run/met-daemon/ipc.sock)
//!
//! On stdin it accepts the debug commands the Python backend accepted:
//! action names (`start`, `stop`, `tare`, `purge`, `home`, `info`, ...),
//! plus `reset`, `bootloader`, `pause`, `resume`.

use std::path::PathBuf;
use std::time::Duration;

use met_daemon::emulator::{EmulationData, EmulatorTransport};
use met_daemon::gpio::NoopPins;
use met_daemon::transport::SerialTransport;
use met_daemon::{run, DaemonCommand, DaemonOptions, EspAction, Transport};
use tokio::io::{AsyncBufReadExt, BufReader};

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn build_transport() -> Result<Transport, String> {
    let backend = env_or("BACKEND", default_backend()).to_uppercase();
    match backend.as_str() {
        "EMULATOR" | "EMULATION" => {
            let dir = PathBuf::from(env_or("METICULOUS_EMULATION_DIR", "esp_serial/connection"));
            let speed: u32 = env_or("EMULATION_SPEED", "100").parse().unwrap_or(100);
            let data = EmulationData::load(&dir).map_err(|e| e.to_string())?;
            tracing::info!(?dir, speed, "starting in emulation mode");
            Ok(Transport::Emulator(EmulatorTransport::new(data, speed)))
        }
        "USB" => {
            let device = env_or("SERIAL_DEVICE", "/dev/ttyUSB0");
            let serial =
                SerialTransport::open(&device, Box::new(NoopPins)).map_err(|e| e.to_string())?;
            Ok(Transport::Serial(serial))
        }
        // Everything else is a proper Fika connection (Python does the same).
        _ => {
            let device = env_or("SERIAL_DEVICE", "/dev/ttymxc0");
            let pins = fika_pins()?;
            let serial = SerialTransport::open(&device, pins).map_err(|e| e.to_string())?;
            Ok(Transport::Serial(serial))
        }
    }
}

fn default_backend() -> &'static str {
    if cfg!(target_os = "linux") {
        "FIKA"
    } else {
        "EMULATOR"
    }
}

#[cfg(target_os = "linux")]
fn fika_pins() -> Result<Box<dyn met_daemon::gpio::ResetPins>, String> {
    met_daemon::gpio::FikaPins::new()
        .map(|pins| Box::new(pins) as Box<dyn met_daemon::gpio::ResetPins>)
        .map_err(|e| format!("cannot request Fika GPIO lines: {e}"))
}

#[cfg(not(target_os = "linux"))]
fn fika_pins() -> Result<Box<dyn met_daemon::gpio::ResetPins>, String> {
    Err("FIKA backend requires Linux GPIO; set BACKEND=EMULATOR or BACKEND=USB".to_string())
}

async fn stdin_commands(commands: tokio::sync::mpsc::Sender<DaemonCommand>) {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let word = line.trim();
        let command = match word {
            "" => continue,
            "reset" => DaemonCommand::Reset { bootloader: false },
            "bootloader" => DaemonCommand::Reset { bootloader: true },
            "pause" => DaemonCommand::Pause,
            "resume" => DaemonCommand::Resume,
            other => match EspAction::from_wire(other) {
                Some(action) => DaemonCommand::Action(action),
                None => {
                    tracing::warn!(input = other, "unknown command");
                    continue;
                }
            },
        };
        if commands.send(command).await.is_err() {
            break;
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let transport = match build_transport() {
        Ok(transport) => transport,
        Err(error) => {
            tracing::error!(%error, "cannot start daemon");
            std::process::exit(1);
        }
    };

    let alive_check_secs: u64 = env_or("ALIVE_CHECK_SECONDS", "60").parse().unwrap_or(60);
    let handle = run(
        transport,
        DaemonOptions {
            alive_check_delay: Duration::from_secs(alive_check_secs),
        },
    );

    let mut events = handle.events.subscribe();
    tokio::spawn(stdin_commands(handle.commands.clone()));

    let socket = met_ipc::socket_path_from_env();
    let (ipc_events, ipc_snapshot, ipc_commands) = (
        handle.events.clone(),
        handle.snapshot.clone(),
        handle.commands.clone(),
    );
    tokio::spawn(async move {
        if let Err(error) = met_ipc::serve(&socket, ipc_events, ipc_snapshot, ipc_commands).await {
            tracing::error!(%error, "IPC server failed");
        }
    });

    loop {
        match events.recv().await {
            Ok(event) => match serde_json::to_string(&event) {
                Ok(json) => tracing::info!(event = %json),
                Err(_) => tracing::info!(?event),
            },
            Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                tracing::warn!(missed, "event subscriber lagged");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
    let _ = handle.join.await;
}
