//! Physical button events, either as bare tokens (`CCW`) or `Event,...`
//! messages. Mirrors `esp_serial.data.ButtonEventEnum` / `ButtonEventData`.

use serde::Serialize;
use serde_json::{json, Value};

use crate::pynum::py_int;

/// Events the machine's physical controls can emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[allow(missing_docs)]
pub enum ButtonEvent {
    EncoderClockwise,
    EncoderCounterclockwise,
    EncoderPush,
    EncoderDouble,
    EncoderLong,
    Tare,
    TareDouble,
    TareLong,
    TareSuperLong,
    Context,
    EncoderPressed,
    EncoderReleased,
    TarePressed,
    TareReleased,
    ContextPressed,
    ContextReleased,
    Unknown,
}

impl ButtonEvent {
    /// The Python enum member name, used over socket.io.
    pub fn name(self) -> &'static str {
        match self {
            ButtonEvent::EncoderClockwise => "ENCODER_CLOCKWISE",
            ButtonEvent::EncoderCounterclockwise => "ENCODER_COUNTERCLOCKWISE",
            ButtonEvent::EncoderPush => "ENCODER_PUSH",
            ButtonEvent::EncoderDouble => "ENCODER_DOUBLE",
            ButtonEvent::EncoderLong => "ENCODER_LONG",
            ButtonEvent::Tare => "TARE",
            ButtonEvent::TareDouble => "TARE_DOUBLE",
            ButtonEvent::TareLong => "TARE_LONG",
            ButtonEvent::TareSuperLong => "TARE_SUPER_LONG",
            ButtonEvent::Context => "CONTEXT",
            ButtonEvent::EncoderPressed => "ENCODER_PRESSED",
            ButtonEvent::EncoderReleased => "ENCODER_RELEASED",
            ButtonEvent::TarePressed => "TARE_PRESSED",
            ButtonEvent::TareReleased => "TARE_RELEASED",
            ButtonEvent::ContextPressed => "CONTEXT_PRESSED",
            ButtonEvent::ContextReleased => "CONTEXT_RELEASED",
            ButtonEvent::Unknown => "UNKNOWN",
        }
    }

    /// Mirror of Python `ButtonEventEnum.from_str`: first a case-sensitive
    /// lookup of the wire tokens, then a case-insensitive match on the enum
    /// member names. `None` where Python raises `KeyError`.
    pub fn from_wire(token: &str) -> Option<Self> {
        let mapped = match token {
            "CW" => "ENCODER_CLOCKWISE",
            "CCW" => "ENCODER_COUNTERCLOCKWISE",
            "push" => "ENCODER_PUSH",
            "pu_d" => "ENCODER_DOUBLE",
            "elng" => "ENCODER_LONG",
            "tare" => "TARE",
            "ta_d" => "TARE_DOUBLE",
            "ta_l" => "TARE_LONG",
            "ta_sl" => "TARE_SUPER_LONG",
            "strt" | "cntx" => "CONTEXT",
            "encoder_button_pressed" => "ENCODER_PRESSED",
            "encoder_button_released" => "ENCODER_RELEASED",
            "tare_pressed" => "TARE_PRESSED",
            "tare_released" => "TARE_RELEASED",
            "context_pressed" => "CONTEXT_PRESSED",
            "context_released" => "CONTEXT_RELEASED",
            other => other,
        };
        Self::from_name(&mapped.to_uppercase())
    }

    fn from_name(name: &str) -> Option<Self> {
        let event = match name {
            "ENCODER_CLOCKWISE" => ButtonEvent::EncoderClockwise,
            "ENCODER_COUNTERCLOCKWISE" => ButtonEvent::EncoderCounterclockwise,
            "ENCODER_PUSH" => ButtonEvent::EncoderPush,
            "ENCODER_DOUBLE" => ButtonEvent::EncoderDouble,
            "ENCODER_LONG" => ButtonEvent::EncoderLong,
            "TARE" => ButtonEvent::Tare,
            "TARE_DOUBLE" => ButtonEvent::TareDouble,
            "TARE_LONG" => ButtonEvent::TareLong,
            "TARE_SUPER_LONG" => ButtonEvent::TareSuperLong,
            "CONTEXT" => ButtonEvent::Context,
            "ENCODER_PRESSED" => ButtonEvent::EncoderPressed,
            "ENCODER_RELEASED" => ButtonEvent::EncoderReleased,
            "TARE_PRESSED" => ButtonEvent::TarePressed,
            "TARE_RELEASED" => ButtonEvent::TareReleased,
            "CONTEXT_PRESSED" => ButtonEvent::ContextPressed,
            "CONTEXT_RELEASED" => ButtonEvent::ContextReleased,
            "UNKNOWN" => ButtonEvent::Unknown,
            _ => return None,
        };
        Some(event)
    }
}

/// A button event with the time since the previous one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ButtonEventData {
    /// The decoded event.
    pub event: ButtonEvent,
    /// Milliseconds since the previous event; the firmware caps at
    /// `9999+++`, which Python maps to 10000. Unparsable values become 0.
    pub time_since_last_event: i64,
}

impl ButtonEventData {
    /// Parse the argument list of a bare button token or an `Event,...`
    /// message. `None` where Python returns `None` (unknown event name,
    /// empty argument list).
    pub fn from_args(args: &[&str]) -> Option<Self> {
        let mut time_since_last_event = 0;
        if let Some(raw) = args.get(1) {
            if *raw == "9999+++" {
                time_since_last_event = 10000;
            } else if let Some(v) = py_int(raw) {
                time_since_last_event = v;
            }
        }
        Some(ButtonEventData {
            event: ButtonEvent::from_wire(args.first()?)?,
            time_since_last_event,
        })
    }

    /// The payload of the `button` socket.io event (Python `to_sio`).
    pub fn to_sio(&self) -> Value {
        json!({
            "type": self.event.name(),
            "time_since_last_event": self.time_since_last_event,
        })
    }
}
