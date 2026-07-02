//! The wire contract: newline-delimited JSON over a Unix domain socket.
//!
//! Versioned via the `v` field in the hello frame. v1 is deliberately
//! fire-and-forget: the client sends commands, the server streams events —
//! completion is observed through events (`profile_sent`, `port_released`,
//! `port_resumed`), never through replies. This keeps the Python client a
//! dumb reader loop, the same shape as its current serial reader.

use met_daemon::{MachineEvent, MachineSnapshot};
use serde::{Deserialize, Serialize};

/// Protocol version spoken by this build.
pub const PROTOCOL_VERSION: u32 = 1;

/// Frames the server sends, one JSON object per line.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // frames are serialized immediately
pub enum ServerFrame {
    /// First frame on every connection.
    Hello {
        /// Protocol version; the client must refuse to run on a mismatch.
        v: u32,
        /// Current machine state at connect time.
        snapshot: MachineSnapshot,
    },
    /// One daemon event.
    Event {
        /// The event payload.
        event: MachineEvent,
    },
    /// Answer to [`ClientCommand::GetSnapshot`].
    Snapshot {
        /// Current machine state.
        snapshot: MachineSnapshot,
    },
}

/// Frames the client sends, one JSON object per line. Unknown fields are
/// ignored so v1 clients survive additive changes.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClientCommand {
    /// `action,<name>` (Python `Machine.action`). Names are the ESP wire
    /// tokens: start, stop, tare, purge, home, info, ...
    Action {
        /// The action wire token.
        name: String,
    },
    /// Stream a profile JSON (Python `send_json_with_hash`).
    SendProfile {
        /// The profile object.
        profile: serde_json::Value,
    },
    /// Raw bytes to the ESP, hex-encoded (NVS writes, calibration, ...).
    WriteRaw {
        /// Hex-encoded payload (`binascii.hexlify` on the Python side).
        hex: String,
    },
    /// Reset the ESP (Python `Machine.reset()`).
    Reset {
        /// Hold the ESP in its ROM bootloader.
        #[serde(default)]
        bootloader: bool,
    },
    /// Flashing handshake, step 1 — wait for the `port_released` event.
    ReleasePort {
        /// Put the ESP in the bootloader before releasing.
        #[serde(default)]
        bootloader: bool,
    },
    /// Flashing handshake, step 2 — wait for the `port_resumed` event.
    AcquirePort,
    /// Ask for a fresh [`ServerFrame::Snapshot`].
    GetSnapshot,
}

/// Decode a lowercase/uppercase hex string.
pub fn hex_decode(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let bytes = hex.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        let hi = (*bytes.get(i)? as char).to_digit(16)?;
        let lo = (*bytes.get(i + 1)? as char).to_digit(16)?;
        out.push(u8::try_from(hi * 16 + lo).ok()?);
        i += 2;
    }
    Some(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn commands_parse_from_python_shaped_json() {
        let cases = [
            (
                r#"{"kind":"action","name":"purge"}"#,
                ClientCommand::Action {
                    name: "purge".into(),
                },
            ),
            (
                r#"{"kind":"write_raw","hex":"616374696f6e2c696e666f03"}"#,
                ClientCommand::WriteRaw {
                    hex: "616374696f6e2c696e666f03".into(),
                },
            ),
            (
                r#"{"kind":"reset"}"#,
                ClientCommand::Reset { bootloader: false },
            ),
            (
                r#"{"kind":"release_port","bootloader":true}"#,
                ClientCommand::ReleasePort { bootloader: true },
            ),
            (r#"{"kind":"acquire_port"}"#, ClientCommand::AcquirePort),
            (r#"{"kind":"get_snapshot"}"#, ClientCommand::GetSnapshot),
        ];
        for (raw, expected) in cases {
            let parsed: ClientCommand = serde_json::from_str(raw).unwrap();
            assert_eq!(parsed, expected, "{raw}");
        }
    }

    #[test]
    fn hex_decodes_python_hexlify_output() {
        assert_eq!(
            hex_decode("616374696f6e2c696e666f03").unwrap(),
            b"action,info\x03".to_vec()
        );
        assert_eq!(hex_decode("0A0b").unwrap(), vec![0x0A, 0x0B]);
        assert!(hex_decode("abc").is_none());
        assert!(hex_decode("zz").is_none());
    }
}
