//! Native OS notifications (macOS NotificationCenter, Linux libnotify).
//!
//! The daemon calls [`notify`] after persisting an envelope; the call
//! decides — based on the envelope's `raw_event` and a per-
//! `(session_id, raw_event)` debounce — whether to emit a system
//! notification or stay quiet. Telemetry events
//! (`SessionStart` / `UserPromptSubmit` / `PostToolUse`) are never
//! surfaced as OS notifications; only events that indicate "the
//! human is needed" or "the agent is done" qualify.
//!
//! All implementations exit silently on failure — a missing
//! `osascript` / `notify-send` binary, a denied permission, etc.
//! must never break the persist path.

use std::collections::HashMap;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::envelope::Envelope;

/// Default per-event debounce window. Keeps a noisy session from
/// spamming the system notification UI.
pub const DEBOUNCE: Duration = Duration::from_secs(60);

/// A debouncer for OS notifications keyed by
/// `(session_id, raw_event)`.
#[derive(Debug, Default)]
pub struct Debouncer {
    last_emit: Mutex<HashMap<(String, String), Instant>>,
    window: Duration,
}

impl Debouncer {
    /// Build a debouncer with the [`DEBOUNCE`] window.
    pub fn new() -> Self {
        Self::with_window(DEBOUNCE)
    }

    /// Build a debouncer with an explicit window (used by tests).
    pub fn with_window(window: Duration) -> Self {
        Self {
            last_emit: Mutex::new(HashMap::new()),
            window,
        }
    }

    /// Returns `true` when the caller should emit a notification.
    /// Updates the last-emit timestamp as a side-effect on a hit.
    pub fn should_emit(&self, session_id: &str, raw_event: &str) -> bool {
        let key = (session_id.to_string(), raw_event.to_string());
        let mut last = self.last_emit.lock().expect("debouncer mutex poisoned");
        match last.get(&key).copied() {
            Some(prev) if prev.elapsed() < self.window => false,
            _ => {
                last.insert(key, Instant::now());
                true
            }
        }
    }
}

/// Should this envelope drive a system notification at all?
///
/// Returns `false` for telemetry / lifecycle events; `true` for
/// events that signal "human attention needed" or "session ended".
/// Equivalent to [`classify_attention`] returning `Some`.
pub fn is_user_facing(env: &Envelope) -> bool {
    classify_attention(&env.raw_event).is_some()
}

/// The attention state a hook event implies for the session that
/// produced it. Drives the coloured per-session marker in the ainb-tui
/// session list (`[!]` / `[?]` / `[✓]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertKind {
    /// Agent is blocked waiting for the user to approve something
    /// (permission / command / patch approval). Most urgent.
    NeedsPermission,
    /// Agent asked the user a question or is idle awaiting input.
    WaitingOnUser,
    /// Agent finished its turn — informational, no action required.
    Finished,
}

/// Map a host agent's `raw_event` to the attention state it implies,
/// or `None` for telemetry / lifecycle events that don't warrant a
/// marker. This is the single source of truth for which hook events
/// are "user-facing" — both [`is_user_facing`] (OS notifications) and
/// the ainb-tui per-session marker classify through here so the two
/// surfaces never drift apart.
///
/// Host agents name the same semantic events differently; every
/// supported variant maps to the same [`AlertKind`]. A matcher suffix
/// (e.g. `Notification:idle_prompt`) is stripped before matching.
pub fn classify_attention(raw_event: &str) -> Option<AlertKind> {
    let head = raw_event.split(':').next().unwrap_or(raw_event);
    Some(match head {
        // Blocked on an approval — most urgent.
        "PermissionRequest"
        | "permission_request"
        | "exec_approval_request"
        | "apply_patch_approval_request" => AlertKind::NeedsPermission,
        // Asked the user something / idle awaiting input.
        "Notification" | "notification" | "request_user_input" | "wait_for_user" => {
            AlertKind::WaitingOnUser
        }
        // Turn ended — informational.
        "Stop" | "agentStop" | "agent-turn-complete" | "task_complete" => AlertKind::Finished,
        // Telemetry / lifecycle (PreToolUse, PostToolUse, UserPromptSubmit, …).
        _ => return None,
    })
}

/// Render a short, human-readable title from an envelope.
pub fn render_title(env: &Envelope) -> String {
    let head = env.raw_event.split(':').next().unwrap_or(&env.raw_event);
    let agent = match env.agent.as_str() {
        "claude" => "Claude",
        "codex" => "Codex",
        "copilot" => "Copilot",
        other => other,
    };
    match head {
        "Stop" | "agentStop" | "agent-turn-complete" | "task_complete" => {
            format!("{agent} session finished")
        }
        "Notification" | "notification" | "request_user_input" | "wait_for_user" => {
            format!("{agent} is waiting for you")
        }
        "PermissionRequest"
        | "exec_approval_request"
        | "apply_patch_approval_request"
        | "permission_request" => format!("{agent} needs permission"),
        _ => format!("{agent}: {head}"),
    }
}

/// Render a short body from an envelope — best-effort, falls back
/// to the project + cwd when no friendlier text is available.
pub fn render_body(env: &Envelope) -> String {
    if let Some(msg) = env.payload.get("message").and_then(|v| v.as_str()) {
        return msg.to_string();
    }
    if !env.project.is_empty() {
        return env.project.clone();
    }
    if !env.cwd.is_empty() {
        return env.cwd.clone();
    }
    "(no details)".into()
}

/// Emit a native OS notification if [`is_user_facing`] is true and
/// the [`Debouncer`] allows it.
pub fn notify(env: &Envelope, debouncer: &Debouncer) -> bool {
    if !is_user_facing(env) {
        return false;
    }
    if !debouncer.should_emit(&env.session_id, &env.raw_event) {
        return false;
    }
    let title = render_title(env);
    let body = render_body(env);
    let _ = emit_native(&title, &body);
    true
}

#[cfg(target_os = "macos")]
fn emit_native(title: &str, body: &str) -> std::io::Result<()> {
    // `osascript -e 'display notification "body" with title "title"'`
    // is the simplest macOS path — no external deps required.
    let script = format!(
        "display notification {body} with title {title}",
        body = quote_applescript(body),
        title = quote_applescript(title),
    );
    Command::new("osascript")
        .arg("-e")
        .arg(script)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn emit_native(title: &str, body: &str) -> std::io::Result<()> {
    Command::new("notify-send")
        .arg("--app-name=ainb")
        .arg(title)
        .arg(body)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn emit_native(_title: &str, _body: &str) -> std::io::Result<()> {
    // No native surface on this platform — silent no-op.
    Ok(())
}

#[cfg(target_os = "macos")]
fn quote_applescript(s: &str) -> String {
    // AppleScript string literal: wrap in double quotes, escape `"`
    // and `\`. Keep it minimal — we don't pass user-controlled text
    // beyond hook payloads, but a hostile payload should still not
    // crash osascript.
    let escaped: String = s
        .chars()
        .flat_map(|c| match c {
            '"' | '\\' => vec!['\\', c],
            _ => vec![c],
        })
        .collect();
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env(event: &str, agent: &str) -> Envelope {
        Envelope {
            protocol_version: 1,
            agent: agent.into(),
            raw_event: event.into(),
            session_id: "s-1".into(),
            cwd: "/tmp/x".into(),
            project: "x".into(),
            ts: 1,
            payload: json!({"message": "Input needed"}),
        }
    }

    #[test]
    fn telemetry_events_are_not_user_facing() {
        assert!(!is_user_facing(&env("SessionStart", "claude")));
        assert!(!is_user_facing(&env("UserPromptSubmit", "claude")));
        assert!(!is_user_facing(&env("PostToolUse", "claude")));
    }

    #[test]
    fn claude_attention_events_are_user_facing() {
        assert!(is_user_facing(&env("Stop", "claude")));
        assert!(is_user_facing(&env("Notification:idle_prompt", "claude")));
        assert!(is_user_facing(&env("PermissionRequest", "claude")));
    }

    #[test]
    fn codex_attention_events_are_user_facing() {
        assert!(is_user_facing(&env("agent-turn-complete", "codex")));
        assert!(is_user_facing(&env("request_user_input", "codex")));
        assert!(is_user_facing(&env("exec_approval_request", "codex")));
    }

    #[test]
    fn copilot_attention_events_are_user_facing() {
        assert!(is_user_facing(&env("agentStop", "copilot")));
        assert!(is_user_facing(&env("notification", "copilot")));
    }

    #[test]
    fn classify_attention_maps_each_kind() {
        // Permission / approval (Claude + Codex) → NeedsPermission.
        for e in [
            "PermissionRequest",
            "permission_request",
            "exec_approval_request",
            "apply_patch_approval_request",
        ] {
            assert_eq!(
                classify_attention(e),
                Some(AlertKind::NeedsPermission),
                "{e}"
            );
        }
        // Asked / awaiting input → WaitingOnUser.
        for e in [
            "Notification",
            "Notification:idle_prompt",
            "notification",
            "request_user_input",
            "wait_for_user",
        ] {
            assert_eq!(classify_attention(e), Some(AlertKind::WaitingOnUser), "{e}");
        }
        // Turn ended → Finished.
        for e in ["Stop", "agentStop", "agent-turn-complete", "task_complete"] {
            assert_eq!(classify_attention(e), Some(AlertKind::Finished), "{e}");
        }
        // Telemetry / lifecycle → no marker.
        for e in [
            "PreToolUse",
            "PostToolUse",
            "UserPromptSubmit",
            "SessionStart",
            "",
        ] {
            assert_eq!(classify_attention(e), None, "{e}");
        }
    }

    #[test]
    fn debouncer_blocks_repeats_within_window() {
        let d = Debouncer::with_window(Duration::from_secs(60));
        assert!(d.should_emit("s-1", "Stop"));
        assert!(!d.should_emit("s-1", "Stop"));
        assert!(d.should_emit("s-2", "Stop")); // different session
        assert!(d.should_emit("s-1", "PermissionRequest")); // different event
    }

    #[test]
    fn debouncer_allows_repeat_after_window_elapses() {
        let d = Debouncer::with_window(Duration::from_millis(10));
        assert!(d.should_emit("s-1", "Stop"));
        std::thread::sleep(Duration::from_millis(20));
        assert!(d.should_emit("s-1", "Stop"));
    }

    #[test]
    fn render_title_includes_agent_name() {
        assert_eq!(
            render_title(&env("Stop", "claude")),
            "Claude session finished"
        );
        assert_eq!(
            render_title(&env("agent-turn-complete", "codex")),
            "Codex session finished"
        );
        assert_eq!(
            render_title(&env("Notification:idle_prompt", "claude")),
            "Claude is waiting for you"
        );
    }

    #[test]
    fn render_body_prefers_payload_message_then_project() {
        let mut e = env("Stop", "claude");
        e.payload = json!({"message": "All done"});
        assert_eq!(render_body(&e), "All done");
        e.payload = json!({});
        assert_eq!(render_body(&e), "x");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn applescript_quoting_escapes_dangerous_chars() {
        assert_eq!(quote_applescript(r#"a "b" \ c"#), r#""a \"b\" \\ c""#);
    }
}
