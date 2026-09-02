//! Session chat between host and watchers. Tests cover clamp + payload.

use serde_json::{json, Value};

pub const CHAT_MAX_CHARS: usize = 2000;
pub const CHAT_HISTORY: usize = 40;

pub fn clamp_chat(raw: &str) -> String {
    let t = raw.trim();
    let mut out = String::new();
    let mut n = 0usize;
    for ch in t.chars() {
        if n >= CHAT_MAX_CHARS {
            break;
        }
        out.push(ch);
        n += 1;
    }
    out
}

pub fn chat_from(raw: Option<&str>, session: &str) -> &'static str {
    match raw.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "host" => "host",
        "viewer" => "viewer",
        _ if session.trim().is_empty() => "host",
        _ => "viewer",
    }
}

pub fn already_attributed(raw: Option<&str>) -> bool {
    matches!(
        raw.unwrap_or("").trim().to_ascii_lowercase().as_str(),
        "host" | "viewer"
    )
}

pub fn chat_json(text: &str, from: &str, ts: u64) -> String {
    json!({ "type": "chat", "text": text, "from": from, "ts": ts }).to_string()
}

pub fn history_json(messages: &[String]) -> String {
    let items: Vec<Value> = messages
        .iter()
        .filter_map(|m| serde_json::from_str(m).ok())
        .collect();
    json!({ "type": "chat-history", "messages": items }).to_string()
}

pub fn push_history(log: &mut Vec<String>, msg: String) {
    log.push(msg);
    let extra = log.len().saturating_sub(CHAT_HISTORY);
    if extra > 0 {
        log.drain(0..extra);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_trims_and_caps_chars() {
        assert_eq!(clamp_chat("  hi  "), "hi");
        assert!(clamp_chat("   ").is_empty());
        let long: String = "é".repeat(CHAT_MAX_CHARS + 40);
        let out = clamp_chat(&long);
        assert_eq!(out.chars().count(), CHAT_MAX_CHARS);
        assert!(out.chars().all(|c| c == 'é'));
    }

    #[test]
    fn from_ignores_spoof_when_unattributed_and_uses_session() {
        assert_eq!(chat_from(None, ""), "host");
        assert_eq!(chat_from(Some("nope"), "sess"), "viewer");
        assert_eq!(chat_from(Some("host"), "sess"), "host");
        assert_eq!(chat_from(Some("VIEWER"), ""), "viewer");
        assert!(already_attributed(Some("host")));
        assert!(!already_attributed(Some("ai")));
    }

    #[test]
    fn history_caps_at_forty_and_wraps_json() {
        let mut log = Vec::new();
        for i in 0..45 {
            push_history(&mut log, chat_json(&format!("m{i}"), "host", i as u64));
        }
        assert_eq!(log.len(), CHAT_HISTORY);
        assert!(log[0].contains("m5"));
        let hist = history_json(&log);
        assert!(hist.contains("chat-history"));
        assert!(hist.contains("m44"));
        assert!(!hist.contains("\"m0\""));
    }
}
