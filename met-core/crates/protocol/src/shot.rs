//! `Data,...` messages: one datapoint of the machine in time, used to track
//! a shot. Mirrors `esp_serial.data.ShotData`.

use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::pynum::{
    format_py_float, safe_float, safe_float_with_nan, strip_crlf, unquote, PyFloat,
};

/// Machine state derived from the reported profile (Python `MachineState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[allow(missing_docs)] // variants serialize to their Python string values
pub enum MachineState {
    #[serde(rename = "idle")]
    Idle,
    #[serde(rename = "purge")]
    Purge,
    #[serde(rename = "home")]
    Home,
    #[serde(rename = "brewing")]
    Brewing,
}

/// One machine datapoint in time.
///
/// Field names and order match the Python dataclass. `PyFloat` fields carry
/// the literal string `"NaN"` on unparsable input, exactly like Python's
/// `safe_float_with_nan`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[allow(missing_docs)] // fields mirror the Python dataclass 1:1
pub struct ShotData {
    pub pressure: PyFloat,
    pub flow: PyFloat,
    pub weight: PyFloat,
    pub stable_weight: bool,
    pub temperature: PyFloat,
    pub status: Option<String>,
    pub profile: Option<String>,
    pub time: i64,
    pub profile_time: i64,
    pub state: MachineState,
    pub is_extracting: bool,
    pub gravimetric_flow: PyFloat,
    pub main_controller_kind: Option<String>,
    pub main_setpoint: f64,
    pub aux_controller_kind: Option<String>,
    pub aux_setpoint: f64,
    pub is_aux_controller_active: bool,
}

impl ShotData {
    /// Parse the comma-separated argument list following the `Data,` prefix.
    ///
    /// Python quirks preserved:
    /// - fewer than 4 arguments raises in Python (uncaught `IndexError` on
    ///   `args[3]`); here it is a graceful `None`.
    /// - the controller block (args 7..=12) is only read when there are more
    ///   than 12 arguments, and a parse failure mid-block keeps whatever was
    ///   already assigned (Python assigns sequentially inside one `try`).
    /// - `status`/`profile` are URL-decoded; a missing argument leaves them
    ///   `None` without failing the datapoint.
    pub fn from_args(args: &[&str]) -> Option<Self> {
        let status = args.get(5).map(|s| unquote(strip_crlf(s)));
        let profile = args.get(6).map(|s| unquote(strip_crlf(s)));

        let stable_weight = strip_crlf(args.get(3)?) == "S";

        let mut main_controller_kind: Option<String> = None;
        let mut main_setpoint = 0.0;
        let mut aux_controller_kind: Option<String> = None;
        let mut aux_setpoint = 0.0;
        let mut is_aux_controller_active = false;
        let mut gravimetric_flow = PyFloat::Num(0.0);

        if args.len() > 12 {
            // Sequential assignment with early exit, mirroring the partial
            // state Python keeps when a float inside the block fails.
            'controllers: {
                main_controller_kind = controller_kind(args.get(7));
                match args.get(8).and_then(|s| safe_float(strip_crlf(s))) {
                    Some(v) => main_setpoint = v,
                    None => break 'controllers,
                }
                aux_controller_kind = controller_kind(args.get(9));
                match args.get(10).and_then(|s| safe_float(strip_crlf(s))) {
                    Some(v) => aux_setpoint = v,
                    None => break 'controllers,
                }
                is_aux_controller_active = args.get(11).map(|s| strip_crlf(s)) == Some("true");
                gravimetric_flow = safe_float_with_nan(args.get(12).unwrap_or(&""));
            }
        }

        let state = match &profile {
            None => MachineState::Idle,
            Some(p) => match p.as_str() {
                "idle" => MachineState::Idle,
                "Purge" => MachineState::Purge,
                "Home" => MachineState::Home,
                _ => MachineState::Brewing,
            },
        };

        Some(ShotData {
            pressure: safe_float_with_nan(args.first()?),
            flow: safe_float_with_nan(args.get(1)?),
            weight: safe_float_with_nan(args.get(2)?),
            stable_weight,
            temperature: safe_float_with_nan(args.get(4)?),
            status,
            profile,
            time: -1,
            profile_time: -1,
            state,
            is_extracting: false,
            gravimetric_flow,
            main_controller_kind,
            main_setpoint,
            aux_controller_kind,
            aux_setpoint,
            is_aux_controller_active,
        })
    }

    /// Mirror of Python `clone_with_time_and_state`: same datapoint stamped
    /// with shot time, extraction state and profile time.
    pub fn clone_with_time_and_state(
        &self,
        time: i64,
        is_extracting: bool,
        profile_time: i64,
    ) -> ShotData {
        ShotData {
            time,
            is_extracting,
            profile_time,
            ..self.clone()
        }
    }

    /// Serialize back to the wire argument list (Python `to_args`), used by
    /// the emulator to rebuild `Data,` lines from fixture data. `state`,
    /// `time` and extraction flags are not part of the wire format.
    pub fn to_args(&self) -> Vec<String> {
        let mut args = vec![
            self.pressure.to_py_string(),
            self.flow.to_py_string(),
            self.weight.to_py_string(),
            if self.stable_weight { "S" } else { "U" }.to_string(),
            self.temperature.to_py_string(),
            self.status.clone().unwrap_or_default(),
            self.profile.clone().unwrap_or_default(),
        ];
        match &self.main_controller_kind {
            Some(kind) => {
                args.push(kind.clone());
                args.push(format_py_float(self.main_setpoint));
            }
            None => {
                args.push("none".to_string());
                args.push("0.0".to_string());
            }
        }
        match &self.aux_controller_kind {
            Some(kind) => {
                args.push(kind.clone());
                args.push(format_py_float(self.aux_setpoint));
                args.push(
                    if self.is_aux_controller_active {
                        "true"
                    } else {
                        "false"
                    }
                    .to_string(),
                );
            }
            None => {
                args.push("none".to_string());
                args.push("0.0".to_string());
                args.push("false".to_string());
            }
        }
        args.push(self.gravimetric_flow.to_py_string());
        args
    }

    /// The payload of the `status` socket.io event (Python `to_sio`).
    pub fn to_sio(&self) -> Value {
        let mut setpoints = Map::new();
        setpoints.insert("active".to_string(), Value::Null);
        if let Some(kind) = &self.main_controller_kind {
            let key = kind.to_lowercase();
            setpoints.insert(key.clone(), json!(self.main_setpoint));
            setpoints.insert("active".to_string(), Value::String(key));
        }
        if let Some(kind) = &self.aux_controller_kind {
            let key = kind.to_lowercase();
            setpoints.insert(key.clone(), json!(self.aux_setpoint));
            if self.is_aux_controller_active {
                setpoints.insert("active".to_string(), Value::String(key));
            }
        }
        json!({
            "name": self.status,
            "sensors": {
                "p": self.pressure,
                "f": self.flow,
                "w": self.weight,
                "t": self.temperature,
                "g": self.gravimetric_flow,
            },
            "setpoints": setpoints,
            "time": self.time,
            "profile": self.profile,
            "profile_time": self.profile_time,
            "state": self.state,
            "extracting": self.is_extracting,
        })
    }
}

fn controller_kind(arg: Option<&&str>) -> Option<String> {
    let kind = strip_crlf(arg?);
    if kind == "none" {
        None
    } else {
        Some(kind.to_string())
    }
}

impl Default for ShotData {
    /// Python dataclass defaults.
    fn default() -> Self {
        ShotData {
            pressure: PyFloat::Num(0.0),
            flow: PyFloat::Num(0.0),
            weight: PyFloat::Num(0.0),
            stable_weight: false,
            temperature: PyFloat::Num(20.0),
            status: Some(String::new()),
            profile: Some(String::new()),
            time: -1,
            profile_time: -1,
            state: MachineState::Idle,
            is_extracting: false,
            gravimetric_flow: PyFloat::Num(0.0),
            main_controller_kind: None,
            main_setpoint: -1.0,
            aux_controller_kind: None,
            aux_setpoint: -1.0,
            is_aux_controller_active: false,
        }
    }
}
