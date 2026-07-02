//! Wire encoding of everything the backend sends *to* the ESP32: actions and
//! hashed profile JSON. Mirrors `Machine.action` / `Machine.send_json_with_hash`.

use md5::{Digest, Md5};
use serde_json::Value;

/// Actions the ESP32 accepts (Python `Machine.ALLOWED_ESP_ACTIONS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum EspAction {
    Start,
    Stop,
    Tare,
    ScaleMasterCalibration,
    Preheat,
    Continue,
    Home,
    Purge,
    Info,
}

impl EspAction {
    /// The wire token.
    pub fn wire_name(self) -> &'static str {
        match self {
            EspAction::Start => "start",
            EspAction::Stop => "stop",
            EspAction::Tare => "tare",
            EspAction::ScaleMasterCalibration => "scale_master_calibration",
            EspAction::Preheat => "preheat",
            EspAction::Continue => "continue",
            EspAction::Home => "home",
            EspAction::Purge => "purge",
            EspAction::Info => "info",
        }
    }

    /// Parse a wire token (used by the CLI/IPC surface).
    pub fn from_wire(token: &str) -> Option<Self> {
        Some(match token {
            "start" => EspAction::Start,
            "stop" => EspAction::Stop,
            "tare" => EspAction::Tare,
            "scale_master_calibration" => EspAction::ScaleMasterCalibration,
            "preheat" => EspAction::Preheat,
            "continue" => EspAction::Continue,
            "home" => EspAction::Home,
            "purge" => EspAction::Purge,
            "info" => EspAction::Info,
            _ => return None,
        })
    }
}

/// `action,<name>\x03` — Python `Machine.action`.
pub fn encode_action(name: &str) -> Vec<u8> {
    format!("action,{name}\x03").into_bytes()
}

/// Frame a profile JSON exactly like Python `send_json_with_hash`:
/// `hash,<md5>\x03json\n<json>\x03`, where the md5 covers the JSON text.
/// Returns the frame and the hash (reported in the `ProfileSent` event).
///
/// Note: the hash covers the bytes *we* send, so Python's and Rust's JSON
/// key spacing may differ between implementations without breaking the ESP —
/// each frame is self-consistent.
pub fn encode_json_with_hash(json: &Value) -> (Vec<u8>, String) {
    let json_string = json.to_string();
    let hash = format!("{:x}", Md5::digest(json_string.as_bytes()));
    let mut frame = Vec::with_capacity(json_string.len() + hash.len() + 12);
    frame.extend_from_slice(b"hash,");
    frame.extend_from_slice(hash.as_bytes());
    frame.extend_from_slice(b"\x03");
    frame.extend_from_slice(b"json\n");
    frame.extend_from_slice(json_string.as_bytes());
    frame.extend_from_slice(b"\x03");
    (frame, hash)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn action_frame_matches_python() {
        assert_eq!(encode_action("start"), b"action,start\x03");
        assert_eq!(
            encode_action(EspAction::Info.wire_name()),
            b"action,info\x03"
        );
    }

    #[test]
    fn json_hash_matches_python_reference() {
        // Reference computed with CPython hashlib over the same bytes:
        // md5('{"name":"parity"}') via json_data[5:-1].
        let (frame, hash) = encode_json_with_hash(&json!({"name": "parity"}));
        assert_eq!(hash, "4d48a8609e2778855d2de253d7fb3e5d");
        let expected =
            b"hash,4d48a8609e2778855d2de253d7fb3e5d\x03json\n{\"name\":\"parity\"}\x03".to_vec();
        assert_eq!(frame, expected);
    }

    #[test]
    fn hash_covers_exactly_the_json_between_marker_and_terminator() {
        let (frame, hash) = encode_json_with_hash(&json!({"a": [1, 2]}));
        let text = String::from_utf8(frame).unwrap();
        let json_part = text
            .split_once("json\n")
            .unwrap()
            .1
            .strip_suffix('\x03')
            .unwrap();
        assert_eq!(format!("{:x}", Md5::digest(json_part.as_bytes())), hash);
    }
}
