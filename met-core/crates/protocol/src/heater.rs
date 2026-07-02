//! `HeaterTimeoutInfo,...` messages: heater/preheat timeout state.
//! Mirrors `esp_serial.data.HeaterTimeoutInfo`.

use serde::Serialize;
use serde_json::{json, Value};

use crate::pynum::py_float;

/// Heater timeout information from the firmware.
///
/// Field names (including the `coffe` typo) match the Python dataclass —
/// they are part of the wire/persistence contract until both sides change.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HeaterTimeoutInfo {
    /// Time remaining for the profile-end timeout.
    pub coffe_profile_end_remaining: f64,
    /// Total profile-end timeout.
    pub coffe_profile_end_timeout: f64,
    /// Time remaining for the preheat timeout.
    pub preheat_remaining: f64,
    /// Total preheat timeout.
    pub preheat_timeout: f64,
}

impl HeaterTimeoutInfo {
    /// Parse the argument list following the `HeaterTimeoutInfo,` prefix.
    /// Exactly 4 float arguments are required; `None` where Python raises
    /// (`machine.py` catches and logs that).
    pub fn from_args(args: &[&str]) -> Option<Self> {
        if args.len() != 4 {
            return None;
        }
        Some(HeaterTimeoutInfo {
            coffe_profile_end_remaining: py_float(args.first()?)?,
            coffe_profile_end_timeout: py_float(args.get(1)?)?,
            preheat_remaining: py_float(args.get(2)?)?,
            preheat_timeout: py_float(args.get(3)?)?,
        })
    }

    /// Nested dictionary form (Python `to_dict`).
    pub fn to_dict(&self) -> Value {
        json!({
            "coffe_profile_end": {
                "remaining": self.coffe_profile_end_remaining,
                "timeout": self.coffe_profile_end_timeout,
            },
            "preheat": {
                "remaining": self.preheat_remaining,
                "timeout": self.preheat_timeout,
            },
        })
    }
}
