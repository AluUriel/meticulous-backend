//! `Notify,...` messages: text the ESP wants the user to see.
//! Mirrors `esp_serial.data.MachineNotify` and the assembly done in
//! `machine.py` (`re-join args, ";" becomes a newline`).

use serde::Serialize;

/// A user-facing message from the firmware.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MachineNotify {
    /// Notification type discriminator, first argument on the wire.
    #[serde(rename = "notificationType")]
    pub notification_type: String,
    /// Message text; `;` on the wire means a line break.
    pub message: String,
}

impl MachineNotify {
    /// Build from the arguments following the `Notify,` prefix, the way
    /// `machine.py` does: first argument is the type, the rest re-joined
    /// with commas and `;` replaced by newlines. `None` when there are no
    /// arguments (Python raises an uncaught `IndexError` there).
    pub fn from_dispatch_args(args: &[&str]) -> Option<Self> {
        let notification_type = (*args.first()?).to_string();
        let message = args.get(1..).unwrap_or(&[]).join(",").replace(';', "\n");
        Some(MachineNotify {
            notification_type,
            message,
        })
    }
}
