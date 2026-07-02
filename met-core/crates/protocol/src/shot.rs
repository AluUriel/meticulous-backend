//! `Data,...` messages: one datapoint of the machine in time, used to track
//! a shot. Mirrors `esp_serial.data.ShotData`.

use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::pynum::{safe_float, safe_float_with_nan, strip_crlf, unquote, PyFloat};

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
