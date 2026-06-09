// ABOUTME: API-error regex patterns + signal detection.
//
// Daemon mode auto-continues when any pattern matches recent output. Keep
// the set specific — better to miss an error than to spam `continue` into
// a working session.

use lazy_static::lazy_static;
use regex::Regex;

use crate::fleet::types::Signal;

pub struct ErrorPattern {
    pub name: &'static str,
    pub re: Regex,
}

lazy_static! {
    pub static ref API_ERROR_PATTERNS: Vec<ErrorPattern> = vec![
        ErrorPattern {
            name: "rate_limited",
            re: Regex::new(r"(?i)\brate[_ ]limited\b").unwrap()
        },
        ErrorPattern {
            name: "overloaded",
            re: Regex::new(r"(?i)\boverloaded_error\b|\bModel is overloaded\b").unwrap(),
        },
        ErrorPattern {
            name: "internal_server_error",
            re: Regex::new(r"(?i)\binternal_server_error\b").unwrap(),
        },
        ErrorPattern {
            name: "request_timeout",
            re: Regex::new(r"(?i)\brequest timed out\b").unwrap(),
        },
        ErrorPattern {
            name: "socket_hang_up",
            re: Regex::new(r"(?i)\bsocket hang up\b").unwrap(),
        },
        ErrorPattern {
            name: "fetch_failed",
            re: Regex::new(r"(?i)\bAPI Error\b|\bfetch failed\b").unwrap(),
        },
        ErrorPattern {
            name: "connection_reset",
            re: Regex::new(r"(?i)\bECONNRESET\b|\bconnection reset\b").unwrap(),
        },
    ];
}

pub fn detect_error_signals(text: &str, at_ms: i64) -> Vec<Signal> {
    let mut out = Vec::new();
    for p in API_ERROR_PATTERNS.iter() {
        if let Some(m) = p.re.find(text) {
            let start = m.start().saturating_sub(80);
            let end = (m.end() + 200).min(text.len());
            out.push(Signal::ApiError {
                at: at_ms,
                pattern: p.name.to_string(),
                raw: text[start..end].to_string(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_rate_limited() {
        let signals = detect_error_signals("API Error: rate_limited please retry", 0);
        // Both 'rate_limited' and 'fetch_failed' (matches "API Error") fire — that's OK.
        assert!(
            signals.iter().any(
                |s| matches!(s, Signal::ApiError { pattern, .. } if pattern == "rate_limited")
            )
        );
    }

    #[test]
    fn no_false_positive_on_clean_text() {
        let signals = detect_error_signals("normal output", 0);
        assert!(signals.is_empty());
    }
}
