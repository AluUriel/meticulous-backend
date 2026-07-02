//! End-to-end over the real socket: emulator-backed daemon + IPC server on
//! one side, a raw UnixStream client (what the Python client does) on the
//! other. This is the contract test for phase 3.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::time::Duration;

use met_daemon::emulator::{EmulationData, EmulatorTransport};
use met_daemon::{run, DaemonOptions, Transport};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::{unix::OwnedWriteHalf, UnixStream};
use tokio::time::timeout;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../esp_serial/connection")
}

async fn start_stack(socket: &PathBuf) {
    let data = EmulationData::load(&fixture_dir()).expect("fixtures must load");
    let transport = Transport::Emulator(EmulatorTransport::new(data, 8000));
    let handle = run(
        transport,
        DaemonOptions {
            alive_check_delay: Duration::from_secs(120),
        },
    );
    let (events, snapshot, commands) = (
        handle.events.clone(),
        handle.snapshot.clone(),
        handle.commands.clone(),
    );
    let path = socket.clone();
    tokio::spawn(async move {
        met_ipc::serve(&path, events, snapshot, commands)
            .await
            .expect("ipc server");
    });
    // Let the listener bind before clients connect.
    for _ in 0..100 {
        if path_exists(socket) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("socket never appeared at {}", socket.display());
}

fn path_exists(p: &PathBuf) -> bool {
    std::fs::metadata(p).is_ok()
}

fn socket_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("met-ipc-test-{name}-{}.sock", std::process::id()))
}

async fn next_frame(lines: &mut Lines<BufReader<tokio::net::unix::OwnedReadHalf>>) -> Value {
    let line = timeout(Duration::from_secs(10), lines.next_line())
        .await
        .expect("timed out reading frame")
        .expect("socket read failed")
        .expect("socket closed");
    serde_json::from_str(&line).expect("frame must be valid JSON")
}

async fn wait_for(
    lines: &mut Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    what: &str,
    pred: impl Fn(&Value) -> bool,
) -> Value {
    timeout(Duration::from_secs(10), async {
        loop {
            let frame = next_frame(lines).await;
            if pred(&frame) {
                return frame;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

async fn send(write: &mut OwnedWriteHalf, command: Value) {
    let mut payload = command.to_string().into_bytes();
    payload.push(b'\n');
    write.write_all(&payload).await.expect("command write");
}

fn event_type(frame: &Value) -> Option<&str> {
    if frame.get("kind")?.as_str()? != "event" {
        return None;
    }
    frame.get("event")?.get("type")?.as_str()
}

#[tokio::test(flavor = "multi_thread")]
async fn full_contract_flow() {
    let socket = socket_path("contract");
    start_stack(&socket).await;

    let stream = UnixStream::connect(&socket).await.expect("connect");
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();

    // 1. Versioned hello with a snapshot.
    let hello = next_frame(&mut lines).await;
    assert_eq!(hello.get("kind").and_then(Value::as_str), Some("hello"));
    assert_eq!(hello.get("v").and_then(Value::as_u64), Some(1));
    assert!(hello.pointer("/snapshot/data_sensors").is_some());

    // 2. Idle playback events stream in.
    wait_for(&mut lines, "status event", |f| {
        event_type(f) == Some("status")
    })
    .await;
    wait_for(&mut lines, "sensors event", |f| {
        event_type(f) == Some("sensors")
    })
    .await;

    // 3. Commands: purge switches the emulator and flips profile readiness.
    send(
        &mut write_half,
        serde_json::json!({"kind": "action", "name": "purge"}),
    )
    .await;
    wait_for(&mut lines, "purge status", |f| {
        event_type(f) == Some("status")
            && f.pointer("/event/data/status").and_then(Value::as_str) == Some("purge")
    })
    .await;

    // 4. Profile upload reports the md5 (CPython reference hash).
    send(
        &mut write_half,
        serde_json::json!({"kind": "send_profile", "profile": {"name": "parity"}}),
    )
    .await;
    let sent = wait_for(&mut lines, "profile_sent", |f| {
        event_type(f) == Some("profile_sent")
    })
    .await;
    assert_eq!(
        sent.pointer("/event/hash").and_then(Value::as_str),
        Some("4d48a8609e2778855d2de253d7fb3e5d")
    );

    // 5. Raw write path (hex, like Python binascii.hexlify).
    send(
        &mut write_half,
        serde_json::json!({"kind": "write_raw", "hex": "616374696f6e2c686f6d6503"}),
    )
    .await;
    wait_for(&mut lines, "home status after raw action,home", |f| {
        event_type(f) == Some("status")
            && f.pointer("/event/data/profile").and_then(Value::as_str) == Some("Home")
    })
    .await;

    // 6. Snapshot on demand.
    send(&mut write_half, serde_json::json!({"kind": "get_snapshot"})).await;
    let snap = wait_for(&mut lines, "snapshot", |f| {
        f.get("kind").and_then(Value::as_str) == Some("snapshot")
    })
    .await;
    assert_eq!(
        snap.pointer("/snapshot/profile_ready")
            .and_then(Value::as_bool),
        Some(true)
    );

    // 7. Flashing handshake: release (bootloader) then reacquire.
    send(
        &mut write_half,
        serde_json::json!({"kind": "release_port", "bootloader": true}),
    )
    .await;
    let released = wait_for(&mut lines, "port_released", |f| {
        event_type(f) == Some("port_released")
    })
    .await;
    assert_eq!(
        released
            .pointer("/event/bootloader")
            .and_then(Value::as_bool),
        Some(true)
    );

    send(&mut write_half, serde_json::json!({"kind": "acquire_port"})).await;
    wait_for(&mut lines, "port_resumed", |f| {
        event_type(f) == Some("port_resumed")
    })
    .await;
    // Playback continues after the handshake.
    wait_for(&mut lines, "status after resume", |f| {
        event_type(f) == Some("status")
    })
    .await;

    let _ = std::fs::remove_file(&socket);
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_commands_do_not_kill_the_connection() {
    let socket = socket_path("malformed");
    start_stack(&socket).await;

    let stream = UnixStream::connect(&socket).await.expect("connect");
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();
    next_frame(&mut lines).await; // hello

    write_half.write_all(b"this is not json\n").await.unwrap();
    send(
        &mut write_half,
        serde_json::json!({"kind": "action", "name": "not_an_action"}),
    )
    .await;
    send(
        &mut write_half,
        serde_json::json!({"kind": "write_raw", "hex": "zz"}),
    )
    .await;

    // The stream must still be alive afterwards.
    wait_for(&mut lines, "status after garbage", |f| {
        event_type(f) == Some("status")
    })
    .await;
    let _ = std::fs::remove_file(&socket);
}
