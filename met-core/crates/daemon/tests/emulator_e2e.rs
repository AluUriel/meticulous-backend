//! End-to-end: the daemon running against the fixture-replay emulator, the
//! same fixtures the Python emulator uses. Exercises transport, read loop,
//! state machine, events and commands together — no hardware needed.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::time::Duration;

use met_daemon::emulator::{EmulationData, EmulatorTransport};
use met_daemon::{run, DaemonCommand, DaemonOptions, EspAction, MachineEvent, Transport};
use tokio::sync::broadcast;
use tokio::time::timeout;

fn fixture_dir() -> PathBuf {
    // met-core/crates/daemon -> repo root -> esp_serial/connection
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../esp_serial/connection")
}

fn start_daemon() -> met_daemon::DaemonHandle {
    let data = EmulationData::load(&fixture_dir()).expect("fixtures must load");
    // 8000% ≈ 25 ms startup delay, ~1 ms per line: fast but still async.
    let transport = Transport::Emulator(EmulatorTransport::new(data, 8000));
    run(
        transport,
        DaemonOptions {
            alive_check_delay: Duration::from_secs(120),
        },
    )
}

async fn wait_for_event(
    events: &mut broadcast::Receiver<MachineEvent>,
    what: &str,
    mut pred: impl FnMut(&MachineEvent) -> bool,
) -> MachineEvent {
    timeout(Duration::from_secs(10), async {
        loop {
            match events.recv().await {
                Ok(event) if pred(&event) => return event,
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => {
                    panic!("event channel closed while waiting for {what}")
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_playback_reaches_the_event_stream() {
    let handle = start_daemon();
    let mut events = handle.events.subscribe();

    let status = wait_for_event(&mut events, "idle status", |e| {
        matches!(e, MachineEvent::Status { .. })
    })
    .await;
    let MachineEvent::Status { data } = status else {
        unreachable!()
    };
    assert_eq!(data.status.as_deref(), Some("idle"));
    assert!(!data.is_extracting);

    wait_for_event(&mut events, "sensors", |e| {
        matches!(e, MachineEvent::Sensors { .. })
    })
    .await;

    let snapshot = handle.snapshot.borrow().clone();
    // info_ready flips on the first Data message; the ESPInfo line itself
    // never plays — a faithful Python emulator quirk (see emulator.rs).
    assert!(snapshot.info_ready);
    assert!(snapshot.esp_info.is_none());
    assert!(snapshot.sensors.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn start_without_profile_is_refused_then_purge_flows() {
    let handle = start_daemon();
    let mut events = handle.events.subscribe();

    // Wait until playback is live.
    wait_for_event(&mut events, "first status", |e| {
        matches!(e, MachineEvent::Status { .. })
    })
    .await;

    // `start` with no profile: refused, mirrored from Python.
    handle
        .commands
        .send(DaemonCommand::Action(EspAction::Start))
        .await
        .unwrap();
    wait_for_event(&mut events, "profile required", |e| {
        matches!(e, MachineEvent::ProfileRequired)
    })
    .await;

    // `purge` marks the profile ready and switches the emulator source.
    handle
        .commands
        .send(DaemonCommand::Action(EspAction::Purge))
        .await
        .unwrap();
    wait_for_event(&mut events, "profile ready", |e| {
        matches!(e, MachineEvent::ProfileReadyChanged { ready: true })
    })
    .await;
    let purge_status = wait_for_event(
        &mut events,
        "purge status",
        |e| matches!(e, MachineEvent::Status { data } if data.status.as_deref() == Some("purge")),
    )
    .await;
    let MachineEvent::Status { data } = purge_status else {
        unreachable!()
    };
    assert_eq!(data.profile.as_deref(), Some("Purge"));

    // Now `start` goes through and the espresso fixture plays.
    handle
        .commands
        .send(DaemonCommand::Action(EspAction::Start))
        .await
        .unwrap();
    wait_for_event(&mut events, "espresso profile status", |e| {
        matches!(e, MachineEvent::Status { data } if data.profile.as_deref() == Some("Onyx Lychee"))
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn profile_upload_emits_hash_and_readiness() {
    let handle = start_daemon();
    let mut events = handle.events.subscribe();

    wait_for_event(&mut events, "first status", |e| {
        matches!(e, MachineEvent::Status { .. })
    })
    .await;

    handle
        .commands
        .send(DaemonCommand::SendProfile(
            serde_json::json!({"name": "parity"}),
        ))
        .await
        .unwrap();

    let sent = wait_for_event(&mut events, "profile sent", |e| {
        matches!(e, MachineEvent::ProfileSent { .. })
    })
    .await;
    let MachineEvent::ProfileSent { hash } = sent else {
        unreachable!()
    };
    // Same reference hash as the command unit test (CPython hashlib).
    assert_eq!(hash, "4d48a8609e2778855d2de253d7fb3e5d");
    assert!(handle.snapshot.borrow().profile_ready);
}
