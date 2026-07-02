//! In-process ESP32 emulator: replays the same JSON fixtures the Python
//! emulator uses (`esp_serial/connection/emulated.*.json`), reacts to
//! `action,start|stop|purge|home` written by the host, and returns to the
//! idle loop when a playback source is exhausted. Port of
//! `EmulatorSerialConnection` + `EmulationData`.

use std::path::Path;
use std::time::Duration;

use met_protocol::{EspInfo, PyFloat, SensorData, ShotData};
use serde_json::Value;

/// Message cadence of the real emulator: Data + Sensors every 150 ms,
/// so one line every 75 ms (Python `DEFAULT_SLEEP_TIME`).
const DEFAULT_SLEEP: Duration = Duration::from_millis(75);
/// Python sleeps 2 s before starting playback.
const STARTUP_DELAY: Duration = Duration::from_millis(2000);

/// Errors while loading the emulation fixtures.
#[derive(Debug, thiserror::Error)]
pub enum EmulationError {
    /// Fixture file missing/unreadable.
    #[error("cannot read fixture {path}: {source}")]
    Io {
        /// Fixture path.
        path: String,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// Fixture is not valid JSON.
    #[error("fixture {path} is not valid JSON: {source}")]
    Json {
        /// Fixture path.
        path: String,
        /// Underlying error.
        #[source]
        source: serde_json::Error,
    },
}

/// The four playback sources (Python `EmulationData`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Idle,
    Espresso,
    Home,
    Purge,
}

/// Wire lines for every playback source, rebuilt from fixtures the same way
/// `EmulationData.init()` does.
#[derive(Debug, Clone)]
pub struct EmulationData {
    idle: Vec<String>,
    espresso: Vec<String>,
    home: Vec<String>,
    purge: Vec<String>,
}

impl EmulationData {
    /// Load from a directory containing `emulated.{shot,home,purge}.json`.
    pub fn load(dir: &Path) -> Result<Self, EmulationError> {
        let idle_shot = ShotData {
            temperature: PyFloat::Num(23.2),
            status: Some("idle".to_string()),
            profile: Some("idle".to_string()),
            ..ShotData::default()
        };
        let idle_sensors = SensorData {
            external_1: 85.11,
            external_2: 86.27,
            bar_up: 68.73,
            bar_mid_up: 69.02,
            bar_mid_down: 67.75,
            bar_down: 65.44,
            tube: 67.74,
            motor_temp: 29.96,
            lam_temp: 32.78,
            motor_position: 74.0,
            bandheater_power: 15.3,
            bandheater_current: 0.62,
            pressure_sensor: 306.0,
            adc_0: 14.0,
            adc_1: 14.0,
            adc_2: 14.0,
            adc_3: 14.0,
            ..SensorData::default()
        };
        let idle = vec![
            format!("Data,{}", idle_shot.to_args().join(",")),
            format!("Sensors,{}", idle_sensors.to_args().join(",")),
            format!("ESPInfo,{}", EspInfo::default().to_args().join(",")),
        ];
        Ok(EmulationData {
            idle,
            espresso: shot_to_lines(&read_fixture(&dir.join("emulated.shot.json"))?),
            home: shot_to_lines(&read_fixture(&dir.join("emulated.home.json"))?),
            purge: shot_to_lines(&read_fixture(&dir.join("emulated.purge.json"))?),
        })
    }

    fn lines(&self, source: Source) -> &[String] {
        match source {
            Source::Idle => &self.idle,
            Source::Espresso => &self.espresso,
            Source::Home => &self.home,
            Source::Purge => &self.purge,
        }
    }
}

fn read_fixture(path: &Path) -> Result<Value, EmulationError> {
    let display = path.display().to_string();
    let raw = std::fs::read_to_string(path).map_err(|source| EmulationError::Io {
        path: display.clone(),
        source,
    })?;
    serde_json::from_str(&raw).map_err(|source| EmulationError::Json {
        path: display,
        source,
    })
}

/// Port of `EmulationData.shotToEmulation`: one `Data,` + one `Sensors,`
/// line per fixture sample with `time > 0`.
fn shot_to_lines(fixture: &Value) -> Vec<String> {
    let profile_name = fixture
        .get("profile_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let samples = fixture
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut lines = Vec::with_capacity(samples.len() * 2);
    for sample in &samples {
        if sample.get("time").and_then(Value::as_f64).unwrap_or(0.0) <= 0.0 {
            continue;
        }
        let sensor = |key: &str| -> f64 {
            sample
                .get("sensors")
                .and_then(|s| s.get(key))
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
        };
        let shot = sample.get("shot").cloned().unwrap_or(Value::Null);
        let shot_field =
            |key: &str| -> f64 { shot.get(key).and_then(Value::as_f64).unwrap_or(0.0) };

        let sensors = SensorData {
            external_1: sensor("external_1"),
            external_2: sensor("external_2"),
            bar_up: sensor("bar_up"),
            bar_mid_up: sensor("bar_mid_up"),
            bar_mid_down: sensor("bar_mid_down"),
            bar_down: sensor("bar_down"),
            tube: sensor("tube"),
            motor_temp: sensor("motor_temp"),
            lam_temp: sensor("lam_temp"),
            motor_position: sensor("motor_position"),
            motor_speed: sensor("motor_speed"),
            motor_power: sensor("motor_power"),
            motor_current: sensor("motor_current"),
            bandheater_power: sensor("bandheater_power"),
            bandheater_current: sensor("bandheater_current"),
            pressure_sensor: sensor("pressure_sensor"),
            adc_0: sensor("adc_0"),
            adc_1: sensor("adc_1"),
            adc_2: sensor("adc_2"),
            adc_3: sensor("adc_3"),
            water_status: sample
                .get("sensors")
                .and_then(|s| s.get("water_status"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            ..SensorData::default()
        };

        let active = shot
            .get("setpoints")
            .and_then(|s| s.get("active"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let main_setpoint = active
            .as_ref()
            .and_then(|kind| shot.get("setpoints").and_then(|s| s.get(kind)))
            .and_then(Value::as_f64)
            .unwrap_or(-1.0);
        let temperature = shot
            .get("temperature")
            .and_then(Value::as_f64)
            .unwrap_or_else(|| sensor("tube"));

        let data = ShotData {
            pressure: PyFloat::Num(shot_field("pressure")),
            flow: PyFloat::Num(shot_field("flow")),
            weight: PyFloat::Num(shot_field("weight")),
            gravimetric_flow: PyFloat::Num(shot_field("gravimetric_flow")),
            temperature: PyFloat::Num(temperature),
            profile: Some(profile_name.to_string()),
            status: sample
                .get("status")
                .and_then(Value::as_str)
                .map(str::to_string),
            main_controller_kind: active,
            main_setpoint,
            ..ShotData::default()
        };

        lines.push(format!("Data,{}", data.to_args().join(",")));
        lines.push(format!("Sensors,{}", sensors.to_args().join(",")));
    }
    lines
}

/// The emulator transport. Each `read_line` waits one cadence tick and
/// returns the next playback line; bytes written by the host are scanned for
/// `action,` commands exactly like the Python pty thread.
pub struct EmulatorTransport {
    data: EmulationData,
    source: Source,
    line_counter: usize,
    sleep: Duration,
    startup_delay: Option<Duration>,
    host_bytes: Vec<u8>,
}

impl EmulatorTransport {
    /// Create with the Python-equivalent timing, scaled by `speed_percent`
    /// (Python `EMULATION_SPEED`, 100 = real time).
    pub fn new(data: EmulationData, speed_percent: u32) -> Self {
        let factor = f64::from(speed_percent.max(1)) / 100.0;
        EmulatorTransport {
            data,
            source: Source::Idle,
            line_counter: 0,
            sleep: DEFAULT_SLEEP.div_f64(factor),
            startup_delay: Some(STARTUP_DELAY.div_f64(factor)),
            host_bytes: Vec::new(),
        }
    }

    /// Host wrote bytes to the "serial port".
    pub fn write(&mut self, bytes: &[u8]) {
        self.host_bytes.extend_from_slice(bytes);
    }

    /// Python `reset()`: jump past the end so the next read returns to idle.
    pub fn reset(&mut self) {
        tracing::info!("resetting emulated ESP32");
        self.line_counter = usize::MAX / 2;
    }

    /// Return the next line after one cadence tick.
    ///
    /// Faithful Python quirk: the wrap check (`counter >= len - 1`) runs
    /// *before* the line at `len - 1` is played, so the last line of every
    /// source never goes out. In the idle source that last line is the
    /// `ESPInfo,` message — emulated runs therefore never deliver ESPInfo,
    /// exactly like `EmulatorSerialConnection` today.
    pub async fn read_line(&mut self) -> String {
        if let Some(delay) = self.startup_delay.take() {
            tokio::time::sleep(delay).await;
        }

        loop {
            if self.line_counter >= self.data.lines(self.source).len().saturating_sub(1) {
                if self.source != Source::Idle {
                    tracing::info!("emulation source exhausted, returning to idle");
                }
                self.line_counter = 0;
                self.source = Source::Idle;
            }

            if let Some(new_source) = self.consume_host_command() {
                self.source = new_source;
                self.line_counter = 0;
                continue;
            }

            let line = self
                .data
                .lines(self.source)
                .get(self.line_counter)
                .cloned()
                .unwrap_or_default();
            self.line_counter += 1;

            let line = line.trim_matches(|c| " \t\r\n".contains(c)).to_string();
            if line.is_empty() {
                continue;
            }

            tokio::time::sleep(self.sleep).await;
            return format!("{line}\r\n");
        }
    }

    fn consume_host_command(&mut self) -> Option<Source> {
        if self.host_bytes.is_empty() {
            return None;
        }
        let commands = String::from_utf8_lossy(&self.host_bytes).into_owned();
        self.host_bytes.clear();
        if commands.contains("action,start") {
            tracing::info!("starting espresso simulation");
            return Some(Source::Espresso);
        }
        if commands.contains("action,stop") {
            tracing::info!("stopping all simulations, returning to idle");
            return Some(Source::Idle);
        }
        if commands.contains("action,purge") {
            tracing::info!("starting purge simulation");
            return Some(Source::Purge);
        }
        if commands.contains("action,home") {
            tracing::info!("starting home simulation");
            return Some(Source::Home);
        }
        None
    }
}
