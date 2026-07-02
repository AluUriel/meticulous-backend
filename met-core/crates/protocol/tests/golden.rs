//! Golden parity suite: replay UART lines through the Rust parser and
//! compare against the recorded behavior of the Python implementation.
//!
//! Fixtures live in `testdata/goldens/` and are generated from the real
//! Python parsers by `met-core/tools/gen_goldens.py`. Regenerate them
//! whenever `esp_serial/data.py` changes.
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use met_protocol::{has_crash_marker, is_boot_banner, parse_line, EspMessage};
use serde::Deserialize;
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct GoldenCase {
    input: String,
    is_boot_banner: bool,
    has_crash_marker: bool,
    kind: String,
    value: Value,
    sio: Value,
    raises: bool,
}

fn load(name: &str) -> Vec<GoldenCase> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("testdata/goldens")
        .join(name);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("bad golden file {name}: {e}"))
}

fn message_value(message: &EspMessage) -> Value {
    let result = match message {
        EspMessage::Button(b) => serde_json::to_value(serde_json::json!({
            "event": b.event.name(),
            "time_since_last_event": b.time_since_last_event,
        })),
        EspMessage::Shot(s) => serde_json::to_value(s),
        EspMessage::Sensors(s) => serde_json::to_value(s),
        EspMessage::Info(i) => serde_json::to_value(i),
        EspMessage::Notify(n) => serde_json::to_value(n),
        EspMessage::HeaterTimeout(h) => serde_json::to_value(h),
        EspMessage::Log(l) => serde_json::to_value(l),
    };
    #[allow(clippy::unwrap_used)] // test-only; all payloads are serializable
    result.unwrap()
}

fn message_sio(message: &EspMessage) -> Value {
    match message {
        EspMessage::Button(b) => b.to_sio(),
        EspMessage::Shot(s) => s.to_sio(),
        EspMessage::Sensors(s) => s.to_sio_sensors(),
        EspMessage::Info(i) => i.to_sio(),
        EspMessage::Notify(_) => Value::Null,
        EspMessage::HeaterTimeout(h) => h.to_dict(),
        EspMessage::Log(_) => Value::Null,
    }
}

/// Deep equality with numbers compared numerically: Python's `safeFloat`
/// returns the *int* `0` for non-finite input, so its JSON says `0` where
/// Rust says `0.0`. Same JSON number, different lexeme — every consumer
/// (socket.io / JS) treats them identically.
fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(x), Some(y)) => x == y,
            _ => x == y,
        },
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| values_equal(a, b))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| values_equal(v, w)))
        }
        _ => a == b,
    }
}

#[track_caller]
fn assert_values_equal(actual: &Value, expected: &Value, ctx: &str) {
    assert!(
        values_equal(actual, expected),
        "{ctx}\n  rust:   {actual}\n  python: {expected}"
    );
}

fn run_golden_file(name: &str) {
    let cases = load(name);
    assert!(!cases.is_empty(), "{name} is empty — regenerate goldens");
    for case in cases {
        let parsed = parse_line(&case.input);
        let ctx = format!("{name}: input {:?}", case.input);

        assert_eq!(parsed.kind.name(), case.kind, "{ctx}: kind mismatch");
        assert_eq!(
            is_boot_banner(&case.input),
            case.is_boot_banner,
            "{ctx}: boot banner mismatch"
        );
        assert_eq!(
            has_crash_marker(&case.input),
            case.has_crash_marker,
            "{ctx}: crash marker mismatch"
        );

        if case.raises {
            // Python crashed its read loop here; Rust must degrade to None.
            assert!(
                parsed.message.is_none(),
                "{ctx}: Python raises, Rust must return None"
            );
            continue;
        }

        match (&parsed.message, case.value.is_null()) {
            (None, true) => {}
            (Some(message), false) => {
                assert_values_equal(
                    &message_value(message),
                    &case.value,
                    &format!("{ctx}: value mismatch"),
                );
                assert_values_equal(
                    &message_sio(message),
                    &case.sio,
                    &format!("{ctx}: sio mismatch"),
                );
            }
            (None, false) => panic!("{ctx}: Python parsed a value, Rust returned None"),
            (Some(m), true) => panic!("{ctx}: Python returned None, Rust parsed {m:?}"),
        }
    }
}

#[test]
fn golden_fixtures() {
    run_golden_file("fixtures.json");
}

#[test]
fn golden_edge_cases() {
    run_golden_file("edge_cases.json");
}
