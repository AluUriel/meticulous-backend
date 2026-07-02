//! The daemon runtime: owns the transport and the [`ReaderState`], executes
//! effects, publishes events (broadcast) and state (watch), and accepts
//! commands. This is the tokio replacement for the `MachineSerial` thread.

use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::{broadcast, mpsc, watch};

use crate::command::{encode_action, encode_json_with_hash, EspAction};
use crate::events::MachineEvent;
use crate::reader::{Effect, MachineSnapshot, ReaderState};
use crate::transport::{ReadOutcome, Transport};

/// Python: `flashingEsp` waits 60 s before `check_machine_alive`.
const ALIVE_CHECK_DELAY: Duration = Duration::from_secs(60);

/// Commands the outside world (CLI now, IPC in phase 3) can send.
#[derive(Debug)]
pub enum DaemonCommand {
    /// Send an `action,<x>` to the ESP. `Start` without a loaded profile is
    /// refused with a [`MachineEvent::ProfileRequired`] event, mirroring
    /// Python (which fetches the last profile itself — the backend keeps
    /// that responsibility).
    Action(EspAction),
    /// Stream a profile JSON (Python `send_json_with_hash`).
    SendProfile(Value),
    /// Raw passthrough (manufacturing/NVS paths stay backend-side for now).
    WriteRaw(Vec<u8>),
    /// Reset the ESP (Python `Machine.reset()`); `bootloader` holds it in
    /// the ROM loader for flashing.
    Reset {
        /// Enter the serial bootloader instead of the firmware.
        bootloader: bool,
    },
    /// Stop reading/writing the port (Python `Machine._stopESPcomm = True`)
    /// so an external flasher can own it.
    Pause,
    /// Resume after [`DaemonCommand::Pause`].
    Resume,
}

/// Handle to a running daemon.
pub struct DaemonHandle {
    /// Subscribe for [`MachineEvent`]s.
    pub events: broadcast::Sender<MachineEvent>,
    /// Latest machine state.
    pub snapshot: watch::Receiver<MachineSnapshot>,
    /// Send [`DaemonCommand`]s.
    pub commands: mpsc::Sender<DaemonCommand>,
    /// The read-loop task.
    pub join: tokio::task::JoinHandle<()>,
}

/// Options for [`run`].
pub struct DaemonOptions {
    /// Delay before the "ESP never sent info" check; scaled down in tests
    /// and emulation.
    pub alive_check_delay: Duration,
}

impl Default for DaemonOptions {
    fn default() -> Self {
        DaemonOptions {
            alive_check_delay: ALIVE_CHECK_DELAY,
        }
    }
}

/// Start the read loop on the given transport.
pub fn run(transport: Transport, options: DaemonOptions) -> DaemonHandle {
    let (event_tx, _) = broadcast::channel(256);
    let (command_tx, command_rx) = mpsc::channel(64);
    let start = Instant::now();
    let state = ReaderState::new(0);
    let (snapshot_tx, snapshot_rx) = watch::channel(state.snapshot());

    let events = event_tx.clone();
    let join = tokio::spawn(read_loop(
        transport,
        state,
        start,
        event_tx,
        snapshot_tx,
        command_rx,
        options,
    ));

    DaemonHandle {
        events,
        snapshot: snapshot_rx,
        commands: command_tx,
        join,
    }
}

#[allow(clippy::too_many_lines)]
async fn read_loop(
    mut transport: Transport,
    mut state: ReaderState,
    start: Instant,
    events: broadcast::Sender<MachineEvent>,
    snapshot: watch::Sender<MachineSnapshot>,
    mut commands: mpsc::Receiver<DaemonCommand>,
    options: DaemonOptions,
) {
    let now_ms =
        |start: Instant| -> u64 { u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX) };
    let mut paused = false;
    let mut alive_check = Box::pin(tokio::time::sleep(options.alive_check_delay));
    let mut alive_checked = false;

    // Python Machine.init: wake the line discipline and ask for info.
    let _ = transport.write(b"32\n").await;
    let _ = transport.write(b"\x03").await;
    let _ = transport.write(&encode_action("info")).await;

    loop {
        if paused {
            // Python: while _stopESPcomm the loop only sleeps.
            match commands.recv().await {
                None => break,
                Some(command) => {
                    paused =
                        handle_command(command, &mut transport, &mut state, &events, paused).await;
                    snapshot.send_replace(state.snapshot());
                }
            }
            continue;
        }

        tokio::select! {
            command = commands.recv() => match command {
                None => break,
                Some(command) => {
                    paused = handle_command(command, &mut transport, &mut state, &events, paused)
                        .await;
                }
            },
            outcome = transport.read_line() => {
                let effects = match outcome {
                    ReadOutcome::Line(bytes) => match String::from_utf8(bytes) {
                        Ok(line) => state.handle_line(&line, now_ms(start)),
                        Err(error) => {
                            // Python: log and continue on decode failure.
                            tracing::info!(%error, "discarding undecodable line");
                            Vec::new()
                        }
                    },
                    ReadOutcome::Timeout => state.tick(now_ms(start)),
                    ReadOutcome::Error(error) => {
                        tracing::error!(%error, "serial transport failed");
                        state.tick(now_ms(start))
                    }
                };
                apply_effects(effects, &mut transport, &events, paused).await;
            },
            () = &mut alive_check, if !alive_checked => {
                alive_checked = true;
                if !state.info_ready() {
                    // Python flashingEsp -> check_machine_alive: the backend
                    // decides whether to flash.
                    let _ = events.send(MachineEvent::EspSilentAfterBoot);
                }
            },
        }

        snapshot.send_replace(state.snapshot());
    }
    tracing::info!("daemon read loop finished");
}

async fn apply_effects(
    effects: Vec<Effect>,
    transport: &mut Transport,
    events: &broadcast::Sender<MachineEvent>,
    paused: bool,
) {
    for effect in effects {
        match effect {
            Effect::Send(bytes) => {
                // Python Machine.write: dropped while comms are stopped.
                if !paused {
                    if let Err(error) = transport.write(&bytes).await {
                        tracing::error!(%error, "write to ESP failed");
                    }
                }
            }
            Effect::Event(event) => {
                let _ = events.send(event);
            }
        }
    }
}

/// Returns the new `paused` state.
async fn handle_command(
    command: DaemonCommand,
    transport: &mut Transport,
    state: &mut ReaderState,
    events: &broadcast::Sender<MachineEvent>,
    paused: bool,
) -> bool {
    match command {
        DaemonCommand::Action(action) => {
            if action == EspAction::Start && !state.profile_ready() {
                // Python fetches the last profile from disk here; the
                // backend owns profiles, so report and let it retry.
                tracing::warn!("start requested with no profile loaded");
                let _ = events.send(MachineEvent::ProfileRequired);
                return paused;
            }
            if matches!(action, EspAction::Home | EspAction::Purge) {
                for effect in state.set_profile_ready(true) {
                    if let Effect::Event(event) = effect {
                        let _ = events.send(event);
                    }
                }
            }
            tracing::info!(action = action.wire_name(), "sending action");
            if !paused {
                if let Err(error) = transport.write(&encode_action(action.wire_name())).await {
                    tracing::error!(%error, "action write failed");
                }
            }
        }
        DaemonCommand::SendProfile(json) => {
            let (frame, hash) = encode_json_with_hash(&json);
            if !paused {
                match transport.write(&frame).await {
                    Ok(()) => {
                        for effect in state.set_profile_ready(true) {
                            if let Effect::Event(event) = effect {
                                let _ = events.send(event);
                            }
                        }
                        let _ = events.send(MachineEvent::ProfileSent { hash });
                    }
                    Err(error) => tracing::error!(%error, "profile write failed"),
                }
            }
        }
        DaemonCommand::WriteRaw(bytes) => {
            if !paused {
                if let Err(error) = transport.write(&bytes).await {
                    tracing::error!(%error, "raw write failed");
                }
            }
        }
        DaemonCommand::Reset { bootloader } => {
            state.note_reset_requested();
            transport.reset(bootloader).await;
        }
        DaemonCommand::Pause => {
            tracing::info!("pausing ESP communication");
            return true;
        }
        DaemonCommand::Resume => {
            tracing::info!("resuming ESP communication");
            return false;
        }
    }
    paused
}
