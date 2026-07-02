//! ESP32 reset/bootloader control over GPIO. Mirrors
//! `FikaSerialConnection.reset` (`esp_serial/connection/fika_serial_connection.py`):
//! Fika V3+ pin map, boot pin low, chip into reset, release with the boot pin
//! selecting normal or bootloader mode.

use std::time::Duration;

/// Sync, fast pin setters; sleeps between transitions happen in
/// [`reset_sequence`] so transports can await them.
pub trait ResetPins: Send {
    /// Drive the ESP boot-select pin (io0).
    fn set_boot(&mut self, high: bool);
    /// Drive the ESP enable/reset pin.
    fn set_reset(&mut self, high: bool);
}

/// The reset sequence shared by all pin backends (Python `reset()`):
/// both sleeps are 100 ms there.
pub async fn reset_sequence(pins: &mut dyn ResetPins, bootloader: bool) {
    const SLEEP: Duration = Duration::from_millis(100);
    pins.set_boot(false);
    pins.set_reset(true);
    tokio::time::sleep(SLEEP).await;
    pins.set_boot(bootloader);
    pins.set_reset(false);
    if bootloader {
        tokio::time::sleep(SLEEP).await;
        pins.set_boot(false);
    }
}

/// No-op pins for the emulator and non-Fika development hosts.
#[derive(Debug, Default)]
pub struct NoopPins;

impl ResetPins for NoopPins {
    fn set_boot(&mut self, high: bool) {
        tracing::debug!(high, "noop boot pin");
    }
    fn set_reset(&mut self, high: bool) {
        tracing::debug!(high, "noop reset pin");
    }
}

#[cfg(target_os = "linux")]
pub use fika::FikaPins;

/// Fika V3+ GPIO backend (Linux only).
#[cfg(target_os = "linux")]
pub mod fika {
    use super::ResetPins;

    /// Pin map from `FikaSerialConnection.DEFAULT_PINS`:
    /// en=(chip 4, line 9), esp_en=(0, 7), io0=(0, 8), buffer=(3, 26).
    /// `en` and `buffer` are requested (held low) but never toggled, same
    /// as Python.
    pub struct FikaPins {
        boot: gpiod::Lines<gpiod::Output>,
        esp_enable: gpiod::Lines<gpiod::Output>,
        _enable: gpiod::Lines<gpiod::Output>,
        _buffer: gpiod::Lines<gpiod::Output>,
    }

    impl FikaPins {
        /// Request all four lines as outputs.
        /// gpiod's `Result`/`Error` are re-exported `std::io` types.
        pub fn new() -> std::io::Result<Self> {
            let request = |chip_num: usize, line: gpiod::LineId| -> std::io::Result<_> {
                let chip = gpiod::Chip::new(chip_num)?;
                let opts = gpiod::Options::output([line]).consumer("met-daemon");
                chip.request_lines(opts)
            };
            Ok(FikaPins {
                _enable: request(4, 9)?,
                esp_enable: request(0, 7)?,
                boot: request(0, 8)?,
                _buffer: request(3, 26)?,
            })
        }
    }

    impl ResetPins for FikaPins {
        fn set_boot(&mut self, high: bool) {
            if let Err(error) = self.boot.set_values([high]) {
                tracing::error!(%error, "failed to drive io0");
            }
        }
        fn set_reset(&mut self, high: bool) {
            if let Err(error) = self.esp_enable.set_values([high]) {
                tracing::error!(%error, "failed to drive esp_en");
            }
        }
    }
}
