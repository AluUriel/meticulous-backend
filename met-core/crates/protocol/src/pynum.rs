//! Primitives that mirror CPython parsing semantics (`float()`, `int()`,
//! `urllib.parse.unquote`, `str.strip("\r\n")`) closely enough for
//! byte-for-byte parity with `esp_serial/data.py` on protocol input.
//!
//! Known, accepted divergences (documented in met-core/README.md):
//! non-ASCII digits and integers beyond i64 are rejected here while CPython
//! accepts them. The ESP32 only emits ASCII and small integers.

use serde::{Serialize, Serializer};

/// Mirror of CPython `float(s)`. `None` where CPython raises `ValueError`.
pub fn py_float(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let (neg, body) = split_sign(t);
    let lower = body.to_ascii_lowercase();
    let value = match lower.as_str() {
        "inf" | "infinity" => f64::INFINITY,
        "nan" => f64::NAN,
        _ => {
            // inf/infinity/nan spellings were handled above, so the grammars
            // Rust and CPython accept here are identical.
            let cleaned = strip_grouping_underscores(body)?;
            cleaned.parse::<f64>().ok()?
        }
    };
    Some(if neg { -value } else { value })
}

/// Mirror of CPython `int(s)` (base 10), bounded to `i64`.
pub fn py_int(s: &str) -> Option<i64> {
    let t = s.trim();
    let (neg, body) = split_sign(t);
    let cleaned = strip_grouping_underscores(body)?;
    if cleaned.is_empty() || !cleaned.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let v = cleaned.parse::<i64>().ok()?;
    Some(if neg { -v } else { v })
}

fn split_sign(s: &str) -> (bool, &str) {
    if let Some(rest) = s.strip_prefix('-') {
        (true, rest)
    } else if let Some(rest) = s.strip_prefix('+') {
        (false, rest)
    } else {
        (false, s)
    }
}

/// CPython allows `_` in numeric literals only between two digits.
fn strip_grouping_underscores(s: &str) -> Option<String> {
    if !s.contains('_') {
        return Some(s.to_string());
    }
    let bytes = s.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b != b'_' {
            continue;
        }
        let prev_digit = i
            .checked_sub(1)
            .and_then(|p| bytes.get(p))
            .is_some_and(u8::is_ascii_digit);
        let next_digit = bytes.get(i + 1).is_some_and(u8::is_ascii_digit);
        if !prev_digit || !next_digit {
            return None;
        }
    }
    Some(s.replace('_', ""))
}

/// Mirror of `esp_serial.data.safeFloat`: parse like CPython `float()`,
/// clamp non-finite values to `0.0`. `None` where CPython raises.
pub fn safe_float(s: &str) -> Option<f64> {
    py_float(s).map(|v| if v.is_finite() { v } else { 0.0 })
}

/// Value of `esp_serial.data.safe_float_with_nan`: a float, or the literal
/// string `"NaN"` when the input is unparsable or NaN. Serializes exactly
/// like the Python value does in JSON.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PyFloat {
    /// A regular float value.
    Num(f64),
    /// The literal string `"NaN"`.
    NanString,
}

impl Serialize for PyFloat {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            PyFloat::Num(v) => serializer.serialize_f64(*v),
            PyFloat::NanString => serializer.serialize_str("NaN"),
        }
    }
}

/// Mirror of `esp_serial.data.safe_float_with_nan`.
pub fn safe_float_with_nan(s: &str) -> PyFloat {
    match py_float(s) {
        Some(v) if !v.is_nan() => PyFloat::Num(v),
        _ => PyFloat::NanString,
    }
}

/// Mirror of Python `str.strip("\r\n")`.
pub fn strip_crlf(s: &str) -> &str {
    s.trim_matches(|c| c == '\r' || c == '\n')
}

/// Mirror of Python `str(float)` for the value ranges the protocol uses:
/// always keeps a decimal point (`0.0`, not `0`). Divergence (documented in
/// met-core/README.md): CPython switches to e-notation below 1e-4 / above
/// 1e16; this does not, and none of the wire values are in that range.
pub fn format_py_float(v: f64) -> String {
    if v.is_nan() {
        return "nan".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    let s = format!("{v}");
    if s.contains('.') || s.contains('e') {
        s
    } else {
        format!("{s}.0")
    }
}

impl PyFloat {
    /// Mirror of Python `str()` on a `safe_float_with_nan` value.
    pub fn to_py_string(self) -> String {
        match self {
            PyFloat::Num(v) => format_py_float(v),
            PyFloat::NanString => "NaN".to_string(),
        }
    }
}

/// Mirror of `urllib.parse.unquote` with `errors="replace"`: decode `%XX`
/// escapes as UTF-8, leave malformed escapes literal, replace invalid UTF-8.
pub fn unquote(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%' {
            let pair = bytes.get(i + 1).zip(bytes.get(i + 2));
            let decoded = pair.and_then(|(hi, lo)| {
                let hi = (*hi as char).to_digit(16)?;
                let lo = (*lo as char).to_digit(16)?;
                u8::try_from(hi * 16 + lo).ok()
            });
            if let Some(byte) = decoded {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(b);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
