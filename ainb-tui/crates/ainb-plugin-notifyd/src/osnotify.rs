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
use std::future::Future;
use std::pin::Pin;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ainb_hangar_core::channel::{Channel, ChannelSet};

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
///
/// The chosen text is run through [`sanitize_notification_text`] so a hook
/// payload carrying a raw newline or other control character can't produce a
/// malformed `osascript` line (which silently fails to notify) — see that
/// function for the robustness rationale.
pub fn render_body(env: &Envelope) -> String {
    let raw = if let Some(msg) = env.payload.get("message").and_then(|v| v.as_str()) {
        msg.to_string()
    } else if !env.project.is_empty() {
        env.project.clone()
    } else if !env.cwd.is_empty() {
        env.cwd.clone()
    } else {
        "(no details)".into()
    };
    sanitize_notification_text(&raw)
}

/// Collapse control characters (newlines, carriage returns, tabs, and any other
/// C0/DEL control byte) to single spaces for single-line notification text.
///
/// A notification body/title is a single line. On macOS the body is embedded in
/// an AppleScript string literal passed to `osascript -e`; a raw `\n` in that
/// literal yields a multi-line script and the `display notification` call
/// silently fails (no error surfaces — the user just never sees the alert). On
/// Linux `notify-send` is similarly happiest with single-line text. This is pure
/// robustness, not injection defense — quotes are still escaped in
/// [`quote_applescript`] and args are passed via `Command::arg`, never a shell.
fn sanitize_notification_text(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// The concrete surface an OS notification is delivered through. Abstracted so
/// the delivery DECISION ([`notify`]) is unit-testable with a recording stub —
/// no `osascript` / `notify-send` spawns in tests. Production wires
/// [`NativeTransport`]; tests wire a fake that records the calls.
pub trait Transport: Send + Sync {
    /// Deliver one notification. Best-effort — a failure is the surface's own
    /// concern and must never break the daemon's persist path (mirrors
    /// [`emit_native`], which swallows its error).
    fn emit(&self, title: &str, body: &str);
}

/// The production transport: the native OS notification ([`emit_native`]).
#[derive(Debug, Default, Clone, Copy)]
pub struct NativeTransport;

impl Transport for NativeTransport {
    fn emit(&self, title: &str, body: &str) {
        let _ = emit_native(title, body);
    }
}

/// The routing decision for one envelope, as far as notifyd can learn it (tcp T5,
/// agents-in-a-box-fyq).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelResolution {
    /// A daemon answered with the resolved [`ChannelSet`] for this event's kind.
    /// The Os gate applies: an OS notification fires only if the set contains
    /// [`Channel::Os`].
    Known(ChannelSet),
    /// No routing could be learned (no hangar daemon, no matching rule, an
    /// unmapped event, or any transport fault). The caller FAILS OPEN and notifies
    /// exactly as a plain notifyd-only install always has.
    Unknown,
}

/// Resolves the [`ChannelResolution`] for one envelope. The seam that lets
/// notifyd honour the daemon's OS-channel routing without depending on the hangar
/// STORE: production dials the daemon's public `notify_rules_list` RPC (see
/// `crate::resolver`), while tests supply a stub that returns a fixed decision.
pub trait ChannelResolver: Send + Sync {
    /// Resolve the routing decision for `env`. Async because the production
    /// resolver dials a socket; fail-open (`Unknown`) on any fault.
    fn resolve<'a>(
        &'a self,
        env: &'a Envelope,
    ) -> Pin<Box<dyn Future<Output = ChannelResolution> + Send + 'a>>;
}

/// Emit a native OS notification if [`is_user_facing`] is true, the resolved
/// routing does not EXCLUDE the [`Channel::Os`] channel (tcp T5), and the
/// [`Debouncer`] allows it.
///
/// Gate order matters: the Os-exclusion check runs BEFORE the debounce so a
/// suppressed (board-only) event never consumes a debounce slot — a later,
/// genuinely Os-routed event for the same `(session, raw_event)` still fires. An
/// [`ChannelResolution::Unknown`] fails OPEN (notifies), so a plain notifyd-only
/// install with no hangar daemon behaves exactly as before.
pub async fn notify(
    env: &Envelope,
    debouncer: &Debouncer,
    resolver: &dyn ChannelResolver,
    transport: &dyn Transport,
) -> bool {
    if !is_user_facing(env) {
        return false;
    }
    // OS-channel gate: honour the daemon's routing only when it is KNOWN to
    // exclude Os. Unknown → notify (fail-open). Before the debounce (above).
    if let ChannelResolution::Known(set) = resolver.resolve(env).await {
        if !set.contains(Channel::Os) {
            return false;
        }
    }
    if !debouncer.should_emit(&env.session_id, &env.raw_event) {
        return false;
    }
    transport.emit(&render_title(env), &render_body(env));
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
    // AppleScript string literal: wrap in double quotes, escape `"` and `\`, and
    // collapse any control char (newline/CR/tab/…) to a space. A raw newline in
    // the literal would split the `-e` script across lines and make
    // `display notification` silently fail (LOW-7); escaping quotes/backslash
    // keeps a hostile payload from breaking out of the literal. Belt-and-braces:
    // `render_body` already sanitizes, but a `title` reaches here un-sanitized,
    // so the control-char fold also lives here.
    let escaped: String = s
        .chars()
        .flat_map(|c| match c {
            '"' | '\\' => vec!['\\', c],
            c if c.is_control() => vec![' '],
            c => vec![c],
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

    #[test]
    fn render_body_strips_control_chars_to_single_line() {
        // LOW-7: a payload message with a raw newline/CR/tab must come back as a
        // single line so the downstream osascript/notify-send call can't be
        // broken by embedded control characters.
        let mut e = env("Stop", "claude");
        e.payload = json!({"message": "line one\nline two\r\tindented"});
        let body = render_body(&e);
        assert!(!body.contains('\n'), "newline survived: {body:?}");
        assert!(!body.contains('\r'), "carriage return survived: {body:?}");
        assert!(!body.contains('\t'), "tab survived: {body:?}");
        assert_eq!(body, "line one line two  indented");
    }

    /// Records every delivered notification so a test can assert whether the OS
    /// surface was hit — no `osascript` / `notify-send` spawned.
    #[derive(Default)]
    struct StubTransport {
        calls: Mutex<Vec<(String, String)>>,
    }
    impl Transport for StubTransport {
        fn emit(&self, title: &str, body: &str) {
            self.calls.lock().unwrap().push((title.to_string(), body.to_string()));
        }
    }
    impl StubTransport {
        fn count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    /// Returns a fixed [`ChannelResolution`] for every envelope.
    struct StubResolver(ChannelResolution);
    impl ChannelResolver for StubResolver {
        fn resolve<'a>(
            &'a self,
            _env: &'a Envelope,
        ) -> Pin<Box<dyn Future<Output = ChannelResolution> + Send + 'a>> {
            let r = self.0.clone();
            Box::pin(async move { r })
        }
    }

    #[tokio::test]
    async fn os_excluded_channel_set_suppresses_notification() {
        // A resolved set that does NOT contain Os (e.g. a board-only `waiting`
        // row, or phone+web only) suppresses the OS notification entirely.
        let transport = StubTransport::default();
        let resolver = StubResolver(ChannelResolution::Known(ChannelSet::from_channels([
            Channel::Phone,
            Channel::Web,
        ])));
        let fired = notify(
            &env("Stop", "claude"),
            &Debouncer::new(),
            &resolver,
            &transport,
        )
        .await;
        assert!(!fired, "Os-excluded routing must suppress the notification");
        assert_eq!(transport.count(), 0, "transport must not be hit");
    }

    #[tokio::test]
    async fn os_included_channel_set_delivers() {
        let transport = StubTransport::default();
        let resolver = StubResolver(ChannelResolution::Known(ChannelSet::from_channels([
            Channel::Web,
            Channel::Os,
        ])));
        let fired = notify(
            &env("Stop", "claude"),
            &Debouncer::new(),
            &resolver,
            &transport,
        )
        .await;
        assert!(fired, "an Os-included set must deliver");
        assert_eq!(transport.count(), 1, "transport hit exactly once");
    }

    #[tokio::test]
    async fn unknown_resolution_fails_open_and_delivers() {
        // No daemon / no rule / unmapped event → Unknown → notify as before.
        let transport = StubTransport::default();
        let resolver = StubResolver(ChannelResolution::Unknown);
        let fired = notify(
            &env("Notification:idle_prompt", "claude"),
            &Debouncer::new(),
            &resolver,
            &transport,
        )
        .await;
        assert!(fired, "Unknown routing fails open (plain-install behaviour)");
        assert_eq!(transport.count(), 1);
    }

    #[tokio::test]
    async fn telemetry_event_never_delivers_even_with_os() {
        // The is_user_facing gate still wins: a telemetry event never notifies,
        // regardless of a (nonsensical) Os-routed resolution.
        let transport = StubTransport::default();
        let resolver = StubResolver(ChannelResolution::Known(ChannelSet::from_channels([
            Channel::Os,
        ])));
        let fired = notify(
            &env("PostToolUse", "claude"),
            &Debouncer::new(),
            &resolver,
            &transport,
        )
        .await;
        assert!(!fired);
        assert_eq!(transport.count(), 0);
    }

    #[tokio::test]
    async fn os_gate_runs_before_debounce_so_suppression_frees_a_later_send() {
        // A suppressed (Os-excluded) event must NOT consume the debounce slot for
        // its (session, raw_event) key: a subsequent Os-routed event for the same
        // key still fires. This proves the Os gate is evaluated before the debounce.
        let transport = StubTransport::default();
        let debouncer = Debouncer::new(); // 60s window
        let e = env("Stop", "claude");

        let suppressed = notify(
            &e,
            &debouncer,
            &StubResolver(ChannelResolution::Known(ChannelSet::from_channels([Channel::Web]))),
            &transport,
        )
        .await;
        assert!(!suppressed, "Os-excluded → suppressed");

        let delivered = notify(
            &e,
            &debouncer,
            &StubResolver(ChannelResolution::Known(ChannelSet::from_channels([Channel::Os]))),
            &transport,
        )
        .await;
        assert!(
            delivered,
            "the suppressed event freed the debounce slot for the later Os send"
        );
        assert_eq!(transport.count(), 1);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn applescript_quoting_escapes_dangerous_chars() {
        assert_eq!(quote_applescript(r#"a "b" \ c"#), r#""a \"b\" \\ c""#);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn applescript_quoting_folds_control_chars_to_space() {
        // LOW-7: a raw newline in the AppleScript literal would split the -e
        // script and make `display notification` silently fail. Fold to a space.
        assert_eq!(quote_applescript("a\nb\tc"), "\"a b c\"");
    }
}
