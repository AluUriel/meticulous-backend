//! `ESPInfo,...` messages: firmware version and machine identity.
//! Mirrors `esp_serial.data.ESPInfo`.

use serde::Serialize;
use serde_json::{json, Value};

use crate::pynum::{format_py_float, py_float, py_int};

/// ESP firmware and status information.
///
/// Serialized field names match the Python dataclass (camelCase and all).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[allow(missing_docs)] // fields mirror the Python dataclass 1:1
pub struct EspInfo {
    #[serde(rename = "firmwareV")]
    pub firmware_v: String,
    #[serde(rename = "espPinout")]
    pub esp_pinout: i64,
    #[serde(rename = "mainVoltage")]
    pub main_voltage: f64,
    pub color: String,
    #[serde(rename = "serialNumber")]
    pub serial_number: String,
    #[serde(rename = "batchNumber")]
    pub batch_number: String,
    #[serde(rename = "buildDate")]
    pub build_date: String,
    #[serde(rename = "scaleModule")]
    pub scale_module: String,
    #[serde(rename = "partialRetraction")]
    pub partial_retraction: f64,
    #[serde(rename = "autoPurgeAfterShot")]
    pub auto_purge_after_shot: bool,
}

impl EspInfo {
    /// Parse the argument list following the `ESPInfo,` prefix.
    ///
    /// The accepted shapes grew over firmware history; like Python, the
    /// parser branches on argument count (>=10, >=9, >=8, >=3) and ignores
    /// extras. Argument 1 used to be the fan status, so an unparsable value
    /// falls back to pinout 0 instead of failing.
    pub fn from_args(args: &[&str]) -> Option<Self> {
        let esp_pinout = args.get(1).and_then(|s| py_int(s)).unwrap_or(0);
        let firmware_v = (*args.first()?).to_string();
        let main_voltage = py_float(args.get(2)?)?;

        let mut info = EspInfo {
            firmware_v,
            esp_pinout,
            main_voltage,
            color: String::new(),
            serial_number: String::new(),
            batch_number: String::new(),
            build_date: String::new(),
            scale_module: String::new(),
            partial_retraction: 45.0,
            auto_purge_after_shot: false,
        };

        if args.len() >= 8 {
            info.color = (*args.get(3)?).to_string();
            info.serial_number = (*args.get(4)?).to_string();
            info.batch_number = (*args.get(5)?).to_string();
            info.build_date = (*args.get(6)?).to_string();
            info.scale_module = (*args.get(7)?).to_string();
        }
        if args.len() >= 9 {
            info.partial_retraction = py_float(args.get(8)?)?;
        }
        if args.len() >= 10 {
            info.auto_purge_after_shot = args.get(9)?.to_lowercase() == "true";
        }
        Some(info)
    }

    /// Serialize back to the wire argument list (Python `to_args`), used by
    /// the emulator to rebuild `ESPInfo,` lines.
    pub fn to_args(&self) -> Vec<String> {
        vec![
            self.firmware_v.clone(),
            self.esp_pinout.to_string(),
            format_py_float(self.main_voltage),
            self.color.clone(),
            self.serial_number.clone(),
            self.batch_number.clone(),
            self.build_date.clone(),
            self.scale_module.clone(),
            format_py_float(self.partial_retraction),
            if self.auto_purge_after_shot {
                "true"
            } else {
                "false"
            }
            .to_string(),
        ]
    }

    /// The socket.io representation (Python `to_sio`), snake_case keys.
    pub fn to_sio(&self) -> Value {
        json!({
            "firmware_version": self.firmware_v,
            "esp_pinout": self.esp_pinout,
            "main_voltage": self.main_voltage,
            "color": self.color,
            "serial_number": self.serial_number,
            "batch_number": self.batch_number,
            "build_date": self.build_date,
            "scale_module": self.scale_module,
            "partial_retraction": self.partial_retraction,
            "auto_purge_after_shot": self.auto_purge_after_shot,
        })
    }
}

impl Default for EspInfo {
    /// Python dataclass defaults.
    fn default() -> Self {
        EspInfo {
            firmware_v: "0.0.0".to_string(),
            esp_pinout: 0,
            main_voltage: 0.0,
            color: String::new(),
            serial_number: String::new(),
            batch_number: String::new(),
            build_date: String::new(),
            scale_module: String::new(),
            partial_retraction: 45.0,
            auto_purge_after_shot: false,
        }
    }
}
