//! Line-level dispatch: the Rust equivalent of the `match` over
//! comma-separated tokens in `Machine._read_data` (`machine.py`).

use crate::button::ButtonEventData;
use crate::esp_info::EspInfo;
use crate::heater::HeaterTimeoutInfo;
use crate::log::EspLog;
use crate::notify::MachineNotify;
use crate::pynum::strip_crlf;
use crate::sensor::SensorData;
use crate::shot::ShotData;

/// Bare tokens the firmware sends for button events without an `Event,`
/// prefix (kept for firmware backwards compatibility).
pub const BARE_BUTTON_TOKENS: [&str; 8] =
    ["CCW", "CW", "push", "pu_d", "elng", "ta_d", "ta_l", "strt"];

/// Message family a line belongs to, before payload parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum LineKind {
    Button,
    Data,
    Sensors,
    EspInfo,
    Notify,
    HeaterTimeout,
    Log,
    Unrecognized,
}

impl LineKind {
    /// Stable lowercase name, used by the golden test suite.
    pub fn name(self) -> &'static str {
        match self {
            LineKind::Button => "button",
            LineKind::Data => "data",
            LineKind::Sensors => "sensors",
            LineKind::EspInfo => "esp_info",
            LineKind::Notify => "notify",
            LineKind::HeaterTimeout => "heater_timeout",
            LineKind::Log => "log",
            LineKind::Unrecognized => "unrecognized",
        }
    }
}

/// A successfully parsed message payload.
#[derive(Debug, Clone, PartialEq)]
#[allow(missing_docs)]
pub enum EspMessage {
    Button(ButtonEventData),
    Shot(ShotData),
    Sensors(SensorData),
    Info(EspInfo),
    Notify(MachineNotify),
    HeaterTimeout(HeaterTimeoutInfo),
    Log(EspLog),
}

/// The result of dispatching one UART line.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedLine {
    /// Which message family the line matched.
    pub kind: LineKind,
    /// The parsed payload; `None` when the family matched but the payload
    /// was malformed (Python logs a warning and carries on).
    pub message: Option<EspMessage>,
}

/// Dispatch a raw UART line exactly like `machine.py`: strip `\r\n`, split
/// on commas, match on the first token. Never panics; malformed payloads
/// yield `message: None` where Python logs-and-continues or raises into its
/// per-line handler.
pub fn parse_line(raw: &str) -> ParsedLine {
    let line = strip_crlf(raw);
    let tokens: Vec<&str> = line.split(',').collect();

    let (kind, message) = match tokens.as_slice() {
        [token] if BARE_BUTTON_TOKENS.contains(token) => (
            LineKind::Button,
            ButtonEventData::from_args(&[token]).map(EspMessage::Button),
        ),
        ["Event", rest @ ..] => (
            LineKind::Button,
            ButtonEventData::from_args(rest).map(EspMessage::Button),
        ),
        ["Data", rest @ ..] => (
            LineKind::Data,
            ShotData::from_args(rest).map(EspMessage::Shot),
        ),
        ["Sensors", color_coded] => (
            LineKind::Sensors,
            SensorData::from_color_coded_args(color_coded).map(EspMessage::Sensors),
        ),
        ["Sensors", rest @ ..] => (
            LineKind::Sensors,
            SensorData::from_args(rest).map(EspMessage::Sensors),
        ),
        ["ESPInfo", rest @ ..] => (
            LineKind::EspInfo,
            EspInfo::from_args(rest).map(EspMessage::Info),
        ),
        ["Notify", rest @ ..] => (
            LineKind::Notify,
            MachineNotify::from_dispatch_args(rest).map(EspMessage::Notify),
        ),
        ["HeaterTimeoutInfo", rest @ ..] => (
            LineKind::HeaterTimeout,
            HeaterTimeoutInfo::from_args(rest).map(EspMessage::HeaterTimeout),
        ),
        ["Log", rest @ ..] => (LineKind::Log, EspLog::from_args(rest).map(EspMessage::Log)),
        _ => (LineKind::Unrecognized, None),
    };

    ParsedLine { kind, message }
}

/// Whether the raw line is the ESP32 boot banner `machine.py` uses to count
/// resets (`rst:0x...` + `boot:0x...` + `SPI_FAST_FLASH_BOOT`).
pub fn is_boot_banner(raw: &str) -> bool {
    raw.starts_with("rst:0x") && raw.contains("boot:0x") && raw.contains(" (SPI_FAST_FLASH_BOOT)")
}

/// Whether the raw line marks the beginning of an ESP crash dump, after
/// which `machine.py` starts collecting trace lines.
pub fn has_crash_marker(raw: &str) -> bool {
    let lower = raw.to_lowercase();
    ["backtrace", "guru meditation error", "register dump"]
        .iter()
        .any(|marker| lower.contains(marker))
}
