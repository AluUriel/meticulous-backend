//! Property tests: the parser must never panic, no matter what bytes the
//! ESP32 sends. This is the crash class the Rust port exists to remove —
//! malformed UART lines during boot/brownout must degrade to typed errors.

use met_protocol::{has_crash_marker, is_boot_banner, parse_line, EspMessage, LineKind};
use proptest::prelude::*;

proptest! {
    /// Arbitrary unicode garbage never panics.
    #[test]
    fn never_panics_on_arbitrary_input(line in ".*") {
        let _ = parse_line(&line);
        let _ = is_boot_banner(&line);
        let _ = has_crash_marker(&line);
    }

    /// Near-protocol lines (valid prefix, garbage payload) never panic and
    /// always classify under their family.
    #[test]
    fn never_panics_on_near_protocol_input(
        prefix in prop::sample::select(vec![
            "Data", "Sensors", "Event", "ESPInfo", "Notify",
            "HeaterTimeoutInfo", "Log",
        ]),
        payload in prop::collection::vec("[^,]*", 0..30),
    ) {
        let line = format!("{prefix},{}", payload.join(","));
        let parsed = parse_line(&line);
        prop_assert_ne!(parsed.kind, LineKind::Unrecognized);
    }

    /// Well-formed Data lines always parse, and the numeric fields
    /// round-trip through the wire format.
    #[test]
    fn valid_data_lines_parse(
        pressure in -100.0f64..100.0,
        flow in -100.0f64..100.0,
        weight in -1000.0f64..1000.0,
        stable in proptest::bool::ANY,
        temperature in -50.0f64..200.0,
    ) {
        let line = format!(
            "Data,{pressure},{flow},{weight},{},{temperature},brewing,profile",
            if stable { "S" } else { "U" },
        );
        let parsed = parse_line(&line);
        let Some(EspMessage::Shot(shot)) = parsed.message else {
            return Err(TestCaseError::fail(format!("did not parse: {line}")));
        };
        prop_assert_eq!(shot.stable_weight, stable);
        prop_assert_eq!(shot.pressure, met_protocol::PyFloat::Num(pressure));
        prop_assert_eq!(shot.temperature, met_protocol::PyFloat::Num(temperature));
    }

    /// Well-formed Sensors lines (23 numeric args) always parse.
    #[test]
    fn valid_sensor_lines_parse(values in prop::collection::vec(-1000.0f64..1000.0, 20)) {
        let mut args: Vec<String> = values.iter().map(f64::to_string).collect();
        args.push("true".to_string());
        args.push("35.0".to_string());
        args.push("18.5".to_string());
        let line = format!("Sensors,{}", args.join(","));
        let parsed = parse_line(&line);
        prop_assert!(
            matches!(parsed.message, Some(EspMessage::Sensors(_))),
            "did not parse: {}", line
        );
    }
}
