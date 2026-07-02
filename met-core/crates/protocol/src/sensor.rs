//! `Sensors,...` messages: the state of every sensor on the machine.
//! Mirrors `esp_serial.data.SensorData`.

use serde::Serialize;
use serde_json::{json, Value};

use crate::pynum::{safe_float, safe_float_with_nan, PyFloat};

/// Current state of all machine sensors.
///
/// Field names and order match the Python dataclass so serialized values are
/// comparable byte-for-byte in the golden suite.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[allow(missing_docs)] // fields mirror the Python dataclass 1:1
pub struct SensorData {
    pub external_1: f64,
    pub external_2: f64,
    pub bar_up: f64,
    pub bar_mid_up: f64,
    pub bar_mid_down: f64,
    pub bar_down: f64,
    pub tube: f64,
    pub motor_temp: f64,
    pub lam_temp: f64,
    pub motor_position: f64,
    pub motor_speed: f64,
    pub motor_power: f64,
    pub motor_current: f64,
    pub bandheater_power: f64,
    pub bandheater_current: f64,
    pub pressure_sensor: f64,
    pub adc_0: f64,
    pub adc_1: f64,
    pub adc_2: f64,
    pub adc_3: f64,
    pub water_status: bool,
    pub motor_thermistor: PyFloat,
    pub weight_prediction: PyFloat,
}

impl SensorData {
    /// Parse the comma-separated argument list following the `Sensors,`
    /// prefix. `None` where the Python parser logs a warning and returns
    /// `None` (missing fields, unparsable floats).
    ///
    /// Firmware quirk preserved from Python: argument 13 is
    /// `bandheater_current` and 14 is `bandheater_power` — swapped relative
    /// to the field declaration order.
    pub fn from_args(args: &[&str]) -> Option<Self> {
        let f = |i: usize| args.get(i).and_then(|s| safe_float(s));
        Some(SensorData {
            external_1: f(0)?,
            external_2: f(1)?,
            bar_up: f(2)?,
            bar_mid_up: f(3)?,
            bar_mid_down: f(4)?,
            bar_down: f(5)?,
            tube: f(6)?,
            motor_temp: f(7)?,
            lam_temp: f(8)?,
            motor_position: f(9)?,
            motor_speed: f(10)?,
            motor_power: f(11)?,
            motor_current: f(12)?,
            bandheater_current: f(13)?,
            bandheater_power: f(14)?,
            pressure_sensor: f(15)?,
            adc_0: f(16)?,
            adc_1: f(17)?,
            adc_2: f(18)?,
            adc_3: f(19)?,
            water_status: args.get(20)?.to_lowercase() == "true",
            motor_thermistor: safe_float_with_nan(args.get(21)?),
            weight_prediction: safe_float_with_nan(args.get(22)?),
        })
    }

    /// Parse the color-coded single-argument variant, where ANSI-colored
    /// sensor names separate the values instead of commas.
    pub fn from_color_coded_args(color_separated: &str) -> Option<Self> {
        let replaced = strip_color_labels(color_separated);
        let mut args: Vec<&str> = replaced.split(',').collect();
        if args.first() == Some(&"") {
            args.remove(0);
        }
        Self::from_args(&args)
    }

    /// The payload of the `sensors` socket.io event
    /// (Python `to_sio_sensors`).
    pub fn to_sio_sensors(&self) -> Value {
        json!({
            "t_ext_1": self.external_1,
            "t_ext_2": self.external_2,
            "t_bar_up": self.bar_up,
            "t_bar_mu": self.bar_mid_up,
            "t_bar_md": self.bar_mid_down,
            "t_bar_down": self.bar_down,
            "t_tube": self.tube,
            "t_motor_temp": self.motor_temp,
            "lam_temp": self.lam_temp,
            "p": self.pressure_sensor,
            "a_0": self.adc_0,
            "a_1": self.adc_1,
            "a_2": self.adc_2,
            "a_3": self.adc_3,
            "m_pos": self.motor_position,
            "m_spd": self.motor_speed,
            "m_pwr": self.motor_power,
            "m_cur": self.motor_current,
            "bh_pwr": self.bandheater_power,
            "bh_cur": self.bandheater_current,
            "w_stat": self.water_status,
            "motor_temp": self.motor_thermistor,
            "weight_pred": self.weight_prediction,
        })
    }
}

/// Replace each `ESC[1;3Xm <name>ESC[0m` label (X in 1..=6, name in
/// `[a-z0-9_]*`) with a comma — the Rust equivalent of the Python regex
/// `\033\[1;(31|32|33|34|35|36)m [a-z0-9_]*\033\[0m`.
fn strip_color_labels(input: &str) -> String {
    const RESET: &str = "\x1b[0m";
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(pos) = rest.find('\x1b') {
        let (before, candidate) = rest.split_at(pos);
        out.push_str(before);
        match match_color_label(candidate) {
            Some(len) => {
                out.push(',');
                rest = candidate.get(len..).unwrap_or("");
            }
            None => {
                out.push('\x1b');
                rest = candidate.get(1..).unwrap_or("");
            }
        }
    }
    out.push_str(rest);
    return out;

    /// Length of the label if `s` starts with a full color label.
    fn match_color_label(s: &str) -> Option<usize> {
        let after_start = s.strip_prefix("\x1b[1;3")?;
        let color_digit = after_start.chars().next()?;
        if !('1'..='6').contains(&color_digit) {
            return None;
        }
        let after_color = after_start.get(1..)?.strip_prefix("m ")?;
        let name_len = after_color
            .bytes()
            .take_while(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
            .count();
        let after_name = after_color.get(name_len..)?;
        if !after_name.starts_with(RESET) {
            return None;
        }
        let consumed = s.len() - after_name.len() + RESET.len();
        Some(consumed)
    }
}
