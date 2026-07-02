//! Events the daemon publishes. In phase 3 these cross the IPC boundary to
//! the Python backend, which keeps all business reactions (sounds, shot
//! manager, notifications UI, alarms, firmware flashing decisions).

use met_protocol::{
    ButtonEventData, EspInfo, EspLog, HeaterTimeoutInfo, MachineNotify, SensorData, ShotData,
};
use serde::Serialize;

/// Alarms mirrored from Python's `AlarmManager` usage in the read loop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "alarm", rename_all = "snake_case")]
pub enum Alarm {
    /// The ESP rebooted without the backend requesting it. Carries the crash
    /// trace collected since the boot banner (may be empty).
    EspRestart {
        /// Joined `Backtrace`/`Guru Meditation` lines, `\n`-separated.
        trace: String,
    },
    /// No valid message for more than 500 ms outside a requested restart.
    EspDisconnected,
}

/// Everything the read loop tells the outside world.
///
/// Mapping to `machine.py` behavior is noted per variant; the daemon reports,
/// the backend decides.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // events are broadcast, not stored
#[allow(missing_docs)] // `data` payload fields are documented on their variants
pub enum MachineEvent {
    /// A `Data,` line arrived; payload is stamped with shot time and
    /// extraction state (Python `Machine.data_sensors`).
    Status { data: ShotData },
    /// A `Sensors,` line arrived (Python `Machine.sensor_sensors`).
    Sensors { data: SensorData },
    /// `ESPInfo` arrived. Python reacts by syncing config, deciding firmware
    /// updates and manufacturing mode; all of that stays backend-side.
    Info { data: EspInfo },
    /// A physical button event (Python: emitted as the `button` sio event;
    /// `ENCODER_DOUBLE` additionally ends the profile).
    Button { data: ButtonEventData },
    /// The ESP asks to notify the user (Python: NotificationManager).
    Notify { data: MachineNotify },
    /// Heater timeout state (Python: `heater_status` sio event).
    HeaterTimeout { data: HeaterTimeoutInfo },
    /// A firmware log line (Python: re-logged, optionally sent to Sentry).
    EspLog { data: EspLog },

    /// ESP boot banner seen; counts toward the reset watchdog.
    BootBanner {
        /// Consecutive boot banners without a valid message in between.
        reset_count: u32,
    },
    /// Three consecutive resets — Python responds with `startUpdate()`.
    FirmwareUpdateRequested,
    /// 60 s after startup the ESP never sent info — Python responds with
    /// `check_machine_alive()` (flash unless the user disallowed it).
    EspSilentAfterBoot,

    /// A shot started (Python: `ShotManager.start()` + brewing-start sound).
    ShotStarted,
    /// A shot ended (Python: `ShotManager.stop()` + brewing-end sound).
    ShotEnded,
    /// Status returned to idle (Python: idle sound, profileReady reset).
    WentIdle,
    /// Heating began (Python: heating-start sound, timers reset).
    HeatingStarted,
    /// Heating finished (Python: heating-end sound).
    HeatingEnded,

    /// `profileReady` changed (Python: gates `action,start` and the shot
    /// debug manager).
    ProfileReadyChanged { ready: bool },
    /// `start` was requested with no profile loaded; the backend must send
    /// one and retry (Python: sends the last profile from disk itself).
    ProfileRequired,
    /// A profile JSON was streamed to the ESP with this md5.
    ProfileSent { hash: String },

    /// An alarm condition began (Python: `AlarmManager.set_alarm` + Sentry).
    AlarmRaised { alarm: Alarm },
    /// A valid message arrived while alarms were up (Python: `clear_alarm`).
    AlarmsCleared,
}
