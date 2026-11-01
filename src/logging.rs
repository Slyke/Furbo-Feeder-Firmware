//! Bounded, volatile diagnostic log. Request bodies and credentials are never logged.
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{Mutex, OnceLock},
};
const LIMIT: usize = 262_144;
#[derive(Default)]
struct Ring {
    entries: VecDeque<String>,
    bytes: usize,
}
static LOG: OnceLock<Mutex<Ring>> = OnceLock::new();
fn redact(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                let key = key.to_ascii_lowercase();
                if [
                    "password",
                    "passphrase",
                    "secret",
                    "token",
                    "api_key",
                    "private_key",
                    "authorization",
                    "psk",
                ]
                .iter()
                .any(|name| key.contains(name))
                {
                    *value = json!("[redacted]");
                } else {
                    redact(value);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                redact(value);
            }
        }
        _ => {}
    }
}
pub fn record(level: &str, event: &str, mut details: Value) {
    redact(&mut details);
    let line =
        json!({"timestamp":crate::model::now(),"level":level,"event":event,"details":details})
            .to_string();
    if line.len() > 8192 {
        return;
    }
    if let Ok(mut ring) = LOG.get_or_init(|| Mutex::new(Ring::default())).lock() {
        while ring.bytes + line.len() + 1 > LIMIT || ring.entries.len() >= 1024 {
            if let Some(old) = ring.entries.pop_front() {
                ring.bytes -= old.len() + 1;
            } else {
                break;
            }
        }
        ring.bytes += line.len() + 1;
        ring.entries.push_back(line);
    }
}
pub fn download() -> String {
    LOG.get_or_init(|| Mutex::new(Ring::default()))
        .lock()
        .map(|r| {
            let mut out = String::with_capacity(r.bytes);
            for line in &r.entries {
                out.push_str(line);
                out.push('\n');
            }
            out
        })
        .unwrap_or_default()
}
#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => {{
        let message = format!($($arg)*);
        $crate::logging::record("error", "runtime", serde_json::json!({"message":message}));
        eprintln!("{message}");
    }};
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_are_redacted_in_nested_log_data() {
        record(
            "info",
            "redaction-test",
            json!({"password":"do-not-emit-7e81","nested":[{"authorization":"do-not-emit-c192"}],"ok":true}),
        );
        let lines = download();
        assert!(!lines.contains("do-not-emit-7e81"));
        assert!(!lines.contains("do-not-emit-c192"));
        assert!(lines
            .lines()
            .all(|line| serde_json::from_str::<Value>(line).is_ok()));
        assert!(lines.len() <= LIMIT);
    }
}
