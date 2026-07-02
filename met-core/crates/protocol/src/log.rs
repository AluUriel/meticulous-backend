//! `Log,...` messages: firmware log lines, optionally with `key=value`
//! items and a sentry routing flag. Mirrors the `Log` branch of
//! `Machine._read_data` in `machine.py`.

use std::collections::BTreeMap;

use serde::Serialize;

/// A parsed firmware log line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EspLog {
    /// Log level, lowercased (`debug`, `info`, `warning`, `error`, ...).
    pub level: String,
    /// The message itself (second argument, unjoined).
    pub message: String,
    /// Everything after the level, re-joined with commas.
    pub full_message: String,
    /// `key=value` items after the message, minus the `sentry` flag.
    /// `None` when the line had no item arguments at all.
    pub items_filtered: Option<BTreeMap<String, String>>,
    /// Whether machine.py would forward this line to Sentry
    /// (`sentry=true` item, or level `error`).
    pub send_to_sentry: bool,
}

impl EspLog {
    /// Parse the argument list following the `Log,` prefix. `None` where
    /// Python hits an exception that `machine.py` catches and logs (fewer
    /// than two arguments).
    pub fn from_args(args: &[&str]) -> Option<Self> {
        let level = args.first()?.to_lowercase();
        let message = (*args.get(1)?).to_string();
        let full_message = args.get(1..).unwrap_or(&[]).join(",");

        let mut send_to_sentry = false;
        let mut items_filtered = None;
        if args.len() > 2 {
            let mut items: BTreeMap<String, String> = BTreeMap::new();
            for part in args.get(2..).unwrap_or(&[]) {
                let mut split = part.splitn(3, '=');
                let (Some(key), Some(value)) = (split.next(), split.next()) else {
                    continue;
                };
                items
                    .entry(key.to_string())
                    .or_insert_with(|| value.to_string());
            }
            send_to_sentry = items.get("sentry").map(String::as_str) == Some("true");
            items.remove("sentry");
            items_filtered = Some(items);
        }
        send_to_sentry = send_to_sentry || level == "error";

        Some(EspLog {
            level,
            message,
            full_message,
            items_filtered,
            send_to_sentry,
        })
    }
}
