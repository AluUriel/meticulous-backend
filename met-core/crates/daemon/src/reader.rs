//! The read-loop state machine: a pure (sans-IO) port of the transport-layer
//! logic in `Machine._read_data` (`machine.py`). The runtime feeds it lines
//! and clock ticks; it returns effects. All time is injected as milliseconds
//! so every code path is unit-testable without hardware or real time.
//!
//! Business reactions that live *around* the loop in Python (sounds, shot
//! manager, notifications, alarms UI, firmware flashing, config sync) are
//! emitted as [`MachineEvent`]s instead — the backend keeps deciding.

use met_protocol::{has_crash_marker, EspInfo, SensorData};
use met_protocol::{is_boot_banner, parse_line, EspMessage, LineKind, ShotData};
use serde::Serialize;

use crate::events::{Alarm, MachineEvent};

/// Machine status strings as the firmware reports them
/// (Python `MachineStatus`).
pub mod status {
    #![allow(missing_docs)]
    pub const IDLE: &str = "idle";
    pub const HEATING: &str = "heating";
    pub const PURGE: &str = "purge";
    pub const RETRACTING: &str = "retracting";
    pub const CLOSING_VALVE: &str = "closing valve";
    pub const HOME: &str = "home";
    pub const BOOT: &str = "boot";
    pub const STARTING: &str = "starting...";
}

/// After this many consecutive boot banners the firmware is reflashed
/// (Python: `Machine.reset_count >= 3`).
const RESET_COUNT_FOR_UPDATE: u32 = 3;
/// Silence threshold for the disconnect alarm (Python: `> 0.5` seconds).
const DISCONNECT_TIMEOUT_MS: u64 = 500;
/// Stable-weight window that ends a shot during retraction
/// (Python: `Machine.stable_time_threshold = 2.0`).
const STABLE_TIME_THRESHOLD_MS: u64 = 2000;

/// What the runtime must do after feeding the state machine.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Write these bytes to the ESP (e.g. an automatic `action,info`).
    Send(Vec<u8>),
    /// Publish this event.
    Event(MachineEvent),
}

/// Read-only snapshot of the machine state, published over a watch channel
/// (phase 3: serialized across the IPC boundary).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MachineSnapshot {
    /// Last `Data,` payload stamped with time/extraction
    /// (Python `Machine.data_sensors`).
    pub data_sensors: ShotData,
    /// Last `Sensors,` payload (Python `Machine.sensor_sensors`).
    pub sensors: Option<SensorData>,
    /// Last `ESPInfo` (Python `Machine.esp_info`).
    pub esp_info: Option<EspInfo>,
    /// Python `Machine.infoReady`.
    pub info_ready: bool,
    /// Python `Machine.profileReady`.
    pub profile_ready: bool,
    /// Consecutive boot banners without a valid message.
    pub reset_count: u32,
}

/// The state machine. See module docs.
#[derive(Debug)]
pub struct ReaderState {
    // Machine identity/state (Python Machine class attributes).
    data_sensors: ShotData,
    sensors: Option<SensorData>,
    esp_info: Option<EspInfo>,
    info_ready: bool,
    profile_ready: bool,
    reset_count: u32,
    /// Python `Machine.esp_restart_request`: starts `true` (init requests a
    /// reset) and suppresses both alarms until the first valid message.
    restart_requested: bool,

    // _read_data() locals.
    info_requested: bool,
    collecting_trace: bool,
    trace: Vec<String>,
    last_valid_ms: u64,
    old_status: String,
    time_flag: bool,
    shot_start_ms: u64,
    time_passed_ms: i64,
    profile_time_ms: i64,
    stable_start_ms: Option<u64>,

    // Alarm latches (Python: AlarmManager.is_alarm_set guards).
    restart_alarm_set: bool,
    disconnect_alarm_set: bool,
}

impl ReaderState {
    /// A fresh state machine; `now_ms` seeds the healthcheck clock.
    pub fn new(now_ms: u64) -> Self {
        let data_sensors = ShotData {
            status: Some(status::IDLE.to_string()),
            profile: Some(status::IDLE.to_string()),
            ..ShotData::default()
        };
        ReaderState {
            data_sensors,
            sensors: None,
            esp_info: None,
            info_ready: false,
            profile_ready: false,
            reset_count: 0,
            restart_requested: true,
            info_requested: false,
            collecting_trace: false,
            trace: Vec::new(),
            last_valid_ms: now_ms,
            old_status: status::IDLE.to_string(),
            time_flag: false,
            shot_start_ms: now_ms,
            time_passed_ms: 0,
            profile_time_ms: 0,
            stable_start_ms: None,
            restart_alarm_set: false,
            disconnect_alarm_set: false,
        }
    }

    /// Current public state.
    pub fn snapshot(&self) -> MachineSnapshot {
        MachineSnapshot {
            data_sensors: self.data_sensors.clone(),
            sensors: self.sensors.clone(),
            esp_info: self.esp_info.clone(),
            info_ready: self.info_ready,
            profile_ready: self.profile_ready,
            reset_count: self.reset_count,
        }
    }

    /// Whether `ESPInfo` has arrived since the last boot.
    pub fn info_ready(&self) -> bool {
        self.info_ready
    }

    /// Whether a profile is loaded on the ESP (gates `action,start`).
    pub fn profile_ready(&self) -> bool {
        self.profile_ready
    }

    /// The backend/runtime requested an ESP reset (Python `Machine.reset()`),
    /// suppressing restart/disconnect alarms until the ESP talks again.
    pub fn note_reset_requested(&mut self) {
        self.restart_requested = true;
        self.info_ready = false;
        self.profile_ready = false;
    }

    /// Mark a profile as loaded (`send_json_with_hash` / `action,home|purge`).
    pub fn set_profile_ready(&mut self, ready: bool) -> Vec<Effect> {
        if self.profile_ready == ready {
            return Vec::new();
        }
        self.profile_ready = ready;
        vec![Effect::Event(MachineEvent::ProfileReadyChanged { ready })]
    }

    /// Feed one decoded UART line. Mirrors one iteration of the Python loop.
    pub fn handle_line(&mut self, raw: &str, now_ms: u64) -> Vec<Effect> {
        let mut effects = Vec::new();
        let mut is_valid = true;

        if is_boot_banner(raw) {
            self.reset_count += 1;
            self.esp_info = None;
            self.info_requested = false;
            self.info_ready = false;
            effects.extend(self.set_profile_ready(false));
            self.collecting_trace = false;
            is_valid = false;
            effects.push(Effect::Event(MachineEvent::BootBanner {
                reset_count: self.reset_count,
            }));
        }

        if self.reset_count >= RESET_COUNT_FOR_UPDATE {
            effects.push(Effect::Event(MachineEvent::FirmwareUpdateRequested));
            self.reset_count = 0;
        }

        if has_crash_marker(raw) {
            self.collecting_trace = true;
        }
        if self.collecting_trace {
            self.trace.push(raw.to_string());
        }

        if self.info_ready && !self.info_requested && self.esp_info.is_none() {
            effects.push(Effect::Send(crate::command::encode_action("info")));
            self.info_requested = true;
        }

        let parsed = parse_line(raw);
        if parsed.kind == LineKind::Unrecognized {
            is_valid = false;
        }

        match parsed.message {
            Some(EspMessage::Shot(data)) => self.handle_shot(data, now_ms, &mut effects),
            Some(EspMessage::Sensors(sensor)) => {
                self.sensors = Some(sensor.clone());
                effects.push(Effect::Event(MachineEvent::Sensors { data: sensor }));
            }
            Some(EspMessage::Info(info)) => {
                self.esp_info = Some(info.clone());
                self.info_ready = true;
                self.info_requested = false;
                effects.push(Effect::Event(MachineEvent::Info { data: info }));
            }
            Some(EspMessage::Button(button)) => {
                effects.push(Effect::Event(MachineEvent::Button { data: button }));
            }
            Some(EspMessage::Notify(notify)) => {
                effects.push(Effect::Event(MachineEvent::Notify { data: notify }));
            }
            Some(EspMessage::HeaterTimeout(heater)) => {
                effects.push(Effect::Event(MachineEvent::HeaterTimeout { data: heater }));
            }
            Some(EspMessage::Log(log)) => {
                effects.push(Effect::Event(MachineEvent::EspLog { data: log }));
            }
            None => {}
        }

        self.healthcheck(is_valid, now_ms, &mut effects);
        effects
    }

    /// A read timeout elapsed with no line (Python: `readline` returned
    /// `None`); only the healthcheck runs.
    pub fn tick(&mut self, now_ms: u64) -> Vec<Effect> {
        let mut effects = Vec::new();
        self.healthcheck(false, now_ms, &mut effects);
        effects
    }

    /// Port of the `if data is not None:` block — shot lifecycle, timers,
    /// and the stamped `data_sensors` the status event carries.
    fn handle_shot(&mut self, data: ShotData, now_ms: u64, effects: &mut Vec<Effect>) {
        let status_str = data.status.clone().unwrap_or_default();
        let is_idle = status_str == status::IDLE;
        let is_purge = status_str == status::PURGE;
        let is_retracting = status_str == status::RETRACTING;
        let is_preparing = status_str == status::CLOSING_VALVE;
        let was_preparing = self.old_status == status::CLOSING_VALVE;
        let is_heating = status_str == status::HEATING;
        let is_starting = status_str == status::STARTING;

        if was_preparing && status_str != self.old_status {
            // A shot started.
            self.time_flag = true;
            self.shot_start_ms = now_ms;
            effects.push(Effect::Event(MachineEvent::ShotStarted));
        } else if self.time_flag {
            // A shot could have ended.
            if is_idle || is_purge {
                self.time_flag = false;
            }
            if self.old_status == status::RETRACTING && !is_retracting {
                self.time_flag = false;
            }
            if is_retracting {
                match self.stable_start_ms {
                    Some(t0) => {
                        self.time_flag = now_ms.saturating_sub(t0) < STABLE_TIME_THRESHOLD_MS;
                        if !data.stable_weight {
                            self.stable_start_ms = None;
                        }
                    }
                    None => {
                        if data.stable_weight {
                            self.stable_start_ms = Some(now_ms);
                        }
                    }
                }
            }
            if !self.time_flag {
                self.stable_start_ms = None;
                effects.push(Effect::Event(MachineEvent::ShotEnded));
            }
        }

        if is_idle && self.old_status != status::IDLE {
            effects.extend(self.set_profile_ready(false));
            effects.push(Effect::Event(MachineEvent::WentIdle));
        }

        if self.old_status == status::IDLE
            && !is_idle
            && (is_heating || is_preparing || is_retracting || is_starting)
        {
            self.time_passed_ms = 0;
            self.profile_time_ms = 0;
        }

        if is_heating && self.old_status != status::HEATING {
            self.time_passed_ms = 0;
            self.profile_time_ms = 0;
            effects.push(Effect::Event(MachineEvent::HeatingStarted));
        }
        if self.old_status == status::HEATING && !is_heating {
            effects.push(Effect::Event(MachineEvent::HeatingEnded));
        }

        if self.time_flag {
            self.time_passed_ms =
                i64::try_from(now_ms.saturating_sub(self.shot_start_ms)).unwrap_or(i64::MAX);
            // Python adjusts profile_time via ShotManager.handleExtractionEnd
            // during retraction; that correction stays backend-side.
            self.profile_time_ms = self.time_passed_ms;
        }
        self.data_sensors = data.clone_with_time_and_state(
            self.time_passed_ms,
            self.time_flag,
            self.profile_time_ms,
        );

        self.old_status = status_str;
        self.info_ready = true;
        effects.push(Effect::Event(MachineEvent::Status {
            data: self.data_sensors.clone(),
        }));
    }

    /// Port of the healthcheck section at the bottom of the loop.
    fn healthcheck(&mut self, got_valid: bool, now_ms: u64, effects: &mut Vec<Effect>) {
        if got_valid {
            self.last_valid_ms = now_ms;
            self.restart_requested = false;
            self.reset_count = 0;
            if self.restart_alarm_set || self.disconnect_alarm_set {
                self.restart_alarm_set = false;
                self.disconnect_alarm_set = false;
                effects.push(Effect::Event(MachineEvent::AlarmsCleared));
            }
        }

        if self.reset_count > 0 && !self.restart_requested && !self.restart_alarm_set {
            self.restart_alarm_set = true;
            let trace = self.trace.join("\n");
            self.trace.clear();
            effects.push(Effect::Event(MachineEvent::AlarmRaised {
                alarm: Alarm::EspRestart { trace },
            }));
        }

        if now_ms.saturating_sub(self.last_valid_ms) > DISCONNECT_TIMEOUT_MS
            && !self.restart_requested
            && !self.disconnect_alarm_set
        {
            self.disconnect_alarm_set = true;
            effects.push(Effect::Event(MachineEvent::AlarmRaised {
                alarm: Alarm::EspDisconnected,
            }));
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const BOOT: &str = "rst:0x1 (POWERON_RESET),boot:0x13 (SPI_FAST_FLASH_BOOT)";

    fn data_line(status: &str, stable: bool) -> String {
        format!(
            "Data,1.0,0.5,10.0,{},92.0,{},profileX",
            if stable { "S" } else { "U" },
            status
        )
    }

    fn events(effects: &[Effect]) -> Vec<&MachineEvent> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Event(ev) => Some(ev),
                Effect::Send(_) => None,
            })
            .collect()
    }

    fn has_event(effects: &[Effect], pred: impl Fn(&MachineEvent) -> bool) -> bool {
        events(effects).into_iter().any(pred)
    }

    #[test]
    fn three_boot_banners_request_firmware_update() {
        let mut state = ReaderState::new(0);
        let e1 = state.handle_line(BOOT, 10);
        assert!(has_event(&e1, |e| matches!(
            e,
            MachineEvent::BootBanner { reset_count: 1 }
        )));
        let e2 = state.handle_line(BOOT, 20);
        assert!(!has_event(&e2, |e| matches!(
            e,
            MachineEvent::FirmwareUpdateRequested
        )));
        let e3 = state.handle_line(BOOT, 30);
        assert!(has_event(&e3, |e| matches!(
            e,
            MachineEvent::FirmwareUpdateRequested
        )));
        assert_eq!(state.snapshot().reset_count, 0);
    }

    #[test]
    fn valid_message_clears_reset_count_and_restart_request() {
        let mut state = ReaderState::new(0);
        state.handle_line(BOOT, 10);
        assert_eq!(state.snapshot().reset_count, 1);
        state.handle_line(&data_line("idle", false), 20);
        assert_eq!(state.snapshot().reset_count, 0);
    }

    #[test]
    fn restart_alarm_carries_crash_trace() {
        let mut state = ReaderState::new(0);
        // First valid message clears the boot-time restart_requested flag.
        state.handle_line(&data_line("idle", false), 5);
        // Real crash order: trace lines stream out, then the chip reboots.
        state.handle_line("Guru Meditation Error: Core 1 panic'ed", 10);
        state.handle_line("Backtrace: 0x40081234:0x3ffb1234", 11);
        let effects = state.handle_line(BOOT, 12);
        let raised = events(&effects)
            .into_iter()
            .find_map(|e| match e {
                MachineEvent::AlarmRaised {
                    alarm: Alarm::EspRestart { trace },
                } => Some(trace),
                _ => None,
            })
            .expect("restart alarm expected");
        assert!(raised.contains("Guru Meditation"));
        assert!(raised.contains("Backtrace"));
    }

    #[test]
    fn startup_restart_request_suppresses_alarms() {
        let mut state = ReaderState::new(0);
        // Boot banner right after startup: no alarm (esp_restart_request).
        let effects = state.handle_line(BOOT, 10);
        assert!(!has_event(&effects, |e| matches!(
            e,
            MachineEvent::AlarmRaised { .. }
        )));
    }

    #[test]
    fn silence_raises_disconnect_alarm_once() {
        let mut state = ReaderState::new(0);
        state.handle_line(&data_line("idle", false), 5);
        let effects = state.tick(600);
        assert!(has_event(&effects, |e| matches!(
            e,
            MachineEvent::AlarmRaised {
                alarm: Alarm::EspDisconnected
            }
        )));
        assert!(state.tick(700).is_empty(), "alarm must not repeat");
        let effects = state.handle_line(&data_line("idle", false), 800);
        assert!(has_event(&effects, |e| matches!(
            e,
            MachineEvent::AlarmsCleared
        )));
    }

    #[test]
    fn info_is_requested_once_after_first_data() {
        let mut state = ReaderState::new(0);
        let first = state.handle_line(&data_line("idle", false), 5);
        assert!(!first.iter().any(|e| matches!(e, Effect::Send(_))));
        let second = state.handle_line(&data_line("idle", false), 10);
        let sends: Vec<_> = second
            .iter()
            .filter(|e| matches!(e, Effect::Send(_)))
            .collect();
        assert_eq!(sends, vec![&Effect::Send(b"action,info\x03".to_vec())]);
        let third = state.handle_line(&data_line("idle", false), 15);
        assert!(!third.iter().any(|e| matches!(e, Effect::Send(_))));
    }

    #[test]
    fn shot_lifecycle_start_to_stable_end() {
        let mut state = ReaderState::new(0);
        state.handle_line(&data_line("idle", false), 0);
        state.handle_line(&data_line("heating", false), 100);
        state.handle_line(&data_line("closing valve", false), 200);
        let started = state.handle_line(&data_line("brewing", false), 300);
        assert!(has_event(&started, |e| matches!(
            e,
            MachineEvent::ShotStarted
        )));

        // While extracting, status events are stamped with shot time.
        let during = state.handle_line(&data_line("brewing", false), 1300);
        let stamped = events(&during)
            .into_iter()
            .find_map(|e| match e {
                MachineEvent::Status { data } => Some(data.clone()),
                _ => None,
            })
            .unwrap();
        assert!(stamped.is_extracting);
        assert_eq!(stamped.time, 1000);

        // Retract with stable weight: shot ends after the 2 s window.
        state.handle_line(&data_line("retracting", true), 2000);
        let ended = state.handle_line(&data_line("retracting", true), 4100);
        assert!(has_event(&ended, |e| matches!(e, MachineEvent::ShotEnded)));

        // Back to idle: profileReady drops, WentIdle fires.
        let idle = state.handle_line(&data_line("idle", false), 4200);
        assert!(has_event(&idle, |e| matches!(e, MachineEvent::WentIdle)));
        assert!(!state.profile_ready());
    }

    #[test]
    fn shot_ends_when_leaving_retract() {
        let mut state = ReaderState::new(0);
        state.handle_line(&data_line("closing valve", false), 0);
        state.handle_line(&data_line("brewing", false), 100);
        state.handle_line(&data_line("retracting", false), 200);
        let ended = state.handle_line(&data_line("home", false), 300);
        assert!(has_event(&ended, |e| matches!(e, MachineEvent::ShotEnded)));
    }

    #[test]
    fn heating_transitions_emit_events_and_reset_timers() {
        let mut state = ReaderState::new(0);
        state.handle_line(&data_line("idle", false), 0);
        let heat = state.handle_line(&data_line("heating", false), 100);
        assert!(has_event(&heat, |e| matches!(
            e,
            MachineEvent::HeatingStarted
        )));
        let cooled = state.handle_line(&data_line("idle", false), 200);
        assert!(has_event(&cooled, |e| matches!(
            e,
            MachineEvent::HeatingEnded
        )));
    }
}
