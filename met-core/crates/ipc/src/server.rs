//! The IPC server: accepts Unix-socket clients (normally exactly one — the
//! Python backend), streams daemon events to each, and forwards their
//! commands to the daemon.

use std::path::{Path, PathBuf};

use met_daemon::{DaemonCommand, EspAction, MachineEvent, MachineSnapshot};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, watch};

use crate::protocol::{hex_decode, ClientCommand, ServerFrame, PROTOCOL_VERSION};

/// Bind the socket and serve clients forever. The socket file is replaced
/// if it already exists (stale from a previous run). Takes the daemon's
/// channel ends so the daemon task itself stays independently owned.
pub async fn serve(
    path: &Path,
    events: broadcast::Sender<MachineEvent>,
    snapshot: watch::Receiver<MachineSnapshot>,
    commands: mpsc::Sender<DaemonCommand>,
) -> std::io::Result<()> {
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let listener = UnixListener::bind(path)?;
    tracing::info!(socket = %path.display(), "IPC server listening");

    loop {
        let (stream, _) = listener.accept().await?;
        tracing::info!("IPC client connected");
        let client_events = events.subscribe();
        let client_snapshot = snapshot.clone();
        let client_commands = commands.clone();
        tokio::spawn(async move {
            if let Err(error) =
                serve_client(stream, client_events, client_snapshot, client_commands).await
            {
                tracing::info!(%error, "IPC client disconnected");
            }
        });
    }
}

/// Path from `METICULOUS_IPC_SOCKET`, with the same default the Python
/// client uses.
pub fn socket_path_from_env() -> PathBuf {
    std::env::var("METICULOUS_IPC_SOCKET")
        .unwrap_or_else(|_| "/tmp/met-daemon.sock".to_string())
        .into()
}

async fn serve_client(
    stream: UnixStream,
    mut events: broadcast::Receiver<MachineEvent>,
    snapshot: watch::Receiver<MachineSnapshot>,
    commands: mpsc::Sender<DaemonCommand>,
) -> std::io::Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();

    let hello = ServerFrame::Hello {
        v: PROTOCOL_VERSION,
        snapshot: snapshot.borrow().clone(),
    };
    write_frame(&mut write_half, &hello).await?;

    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(event) => {
                    write_frame(&mut write_half, &ServerFrame::Event { event }).await?;
                }
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "IPC client lagged, events dropped");
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
            line = lines.next_line() => {
                let Some(line) = line? else { return Ok(()) };
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<ClientCommand>(&line) {
                    Ok(ClientCommand::GetSnapshot) => {
                        let frame = ServerFrame::Snapshot { snapshot: snapshot.borrow().clone() };
                        write_frame(&mut write_half, &frame).await?;
                    }
                    Ok(command) => {
                        if let Some(daemon_command) = translate(command) {
                            if commands.send(daemon_command).await.is_err() {
                                return Ok(());
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, line, "unparsable IPC command");
                    }
                }
            },
        }
    }
}

/// Map wire commands to daemon commands. `None` for malformed payloads
/// (logged, connection stays up — same spirit as the serial parser).
fn translate(command: ClientCommand) -> Option<DaemonCommand> {
    match command {
        ClientCommand::Action { name } => match EspAction::from_wire(&name) {
            Some(action) => Some(DaemonCommand::Action(action)),
            None => {
                tracing::warn!(name, "unknown action over IPC");
                None
            }
        },
        ClientCommand::SendProfile { profile } => Some(DaemonCommand::SendProfile(profile)),
        ClientCommand::WriteRaw { hex } => match hex_decode(&hex) {
            Some(bytes) => Some(DaemonCommand::WriteRaw(bytes)),
            None => {
                tracing::warn!("write_raw with invalid hex payload");
                None
            }
        },
        ClientCommand::Reset { bootloader } => Some(DaemonCommand::Reset { bootloader }),
        ClientCommand::ReleasePort { bootloader } => {
            Some(DaemonCommand::ReleasePort { bootloader })
        }
        ClientCommand::AcquirePort => Some(DaemonCommand::AcquirePort),
        ClientCommand::GetSnapshot => None, // handled inline
    }
}

async fn write_frame(
    stream: &mut tokio::net::unix::OwnedWriteHalf,
    frame: &ServerFrame,
) -> std::io::Result<()> {
    let mut payload = serde_json::to_vec(frame).map_err(std::io::Error::other)?;
    payload.push(b'\n');
    stream.write_all(&payload).await
}
