// ABOUTME: Channel-agnostic relay core shared by the Telegram + Slack channels.
//
// Ported from the Python `bridge._relay`: resolve the target session from the
// raw message (conductor-prefix routing, degrade-to-default), send it, and
// return a single user-facing reply string. Both channels call exactly this —
// the ONE relay/routing core the goal requires — so their behaviour can never
// diverge. The channel layer owns only transport-specific concerns (auth,
// mention-gating, markdown rendering, message splitting).

use std::time::Duration;

use super::routing::{TargetSession, parse_target_prefix, resolve_target};

/// The transport seam the relay drives. The real implementation shells out to
/// `ainb`/tmux (`transport.rs`); tests substitute an in-memory fake so the
/// routing + degrade logic is verified without a live fleet.
///
/// Uses native `async fn` in traits (stable since Rust 1.75) — the relay only
/// ever calls it through a generic `&T`, never a `dyn` object, so the
/// `async_fn_in_trait` auto-trait caveat does not apply here (the returned
/// futures are awaited inline in `Send` channel tasks).
#[allow(async_fn_in_trait)]
pub trait FleetTransport: Send + Sync {
    /// Currently-running relay targets.
    async fn discover(&self) -> Vec<TargetSession>;
    /// Send `text` to `session` and capture its end-of-turn reply (or `None`
    /// on send failure / timeout).
    async fn send_and_capture(
        &self,
        session: &TargetSession,
        text: &str,
        timeout: Duration,
    ) -> Option<String>;
}

/// Parameters that shape a single relay, supplied per-channel.
pub struct RelayParams<'a> {
    /// Optional configured default target name (used when no `name:` prefix).
    pub default_target: Option<&'a str>,
    /// How long to wait for the session's reply.
    pub response_timeout: Duration,
}

/// Resolve the target, relay `raw_text`, and return a user-facing reply.
///
/// Mirrors the Python `_relay` outcomes exactly:
/// - no running sessions -> "No running ainb sessions to relay to."
/// - `name:` prefix to a missing session -> "No running session named '<n>'."
/// - empty message body -> "(empty message — nothing sent to <target>)"
/// - send ok but no reply in time -> "Sent to <t>, but no reply within <s>s …"
/// - reply captured -> the reply text.
pub async fn relay<T: FleetTransport + ?Sized>(
    transport: &T,
    params: &RelayParams<'_>,
    raw_text: &str,
) -> String {
    let sessions = transport.discover().await;
    if sessions.is_empty() {
        return "No running ainb sessions to relay to.".to_string();
    }

    let names: Vec<String> = sessions.iter().map(|s| s.name.clone()).collect();
    let (mut parsed_name, message) = parse_target_prefix(raw_text, &names);

    // No explicit prefix -> honour a configured default target name if present
    // and actually running (case-insensitive).
    if parsed_name.is_none() {
        if let Some(default) = params.default_target {
            if sessions.iter().any(|s| s.name.eq_ignore_ascii_case(default)) {
                parsed_name = Some(default.to_string());
            }
        }
    }

    let Some(target) = resolve_target(parsed_name.as_deref(), &sessions) else {
        return match parsed_name {
            Some(name) => format!("No running session named {name:?}."),
            None => "No target session available.".to_string(),
        };
    };

    if message.is_empty() {
        return format!("(empty message — nothing sent to {})", target.name);
    }

    match transport.send_and_capture(target, &message, params.response_timeout).await {
        Some(reply) => reply,
        None => format!(
            "Sent to {}, but no reply within {}s (it may still be working).",
            target.name,
            params.response_timeout.as_secs()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FakeTransport {
        sessions: Vec<TargetSession>,
        /// Records (target_name, message) of the last send.
        last_send: Mutex<Option<(String, String)>>,
        reply: Option<String>,
    }

    impl FakeTransport {
        fn new(sessions: Vec<TargetSession>, reply: Option<String>) -> Self {
            Self {
                sessions,
                last_send: Mutex::new(None),
                reply,
            }
        }
    }

    impl FleetTransport for FakeTransport {
        async fn discover(&self) -> Vec<TargetSession> {
            self.sessions.clone()
        }
        async fn send_and_capture(
            &self,
            session: &TargetSession,
            text: &str,
            _timeout: Duration,
        ) -> Option<String> {
            *self.last_send.lock().unwrap() = Some((session.name.clone(), text.to_string()));
            self.reply.clone()
        }
    }

    fn sess(name: &str) -> TargetSession {
        TargetSession::new(
            name,
            format!("tmux-{name}"),
            format!("/cwd/{name}"),
            format!("id-{name}"),
        )
    }

    fn params() -> RelayParams<'static> {
        RelayParams {
            default_target: None,
            response_timeout: Duration::from_secs(300),
        }
    }

    #[tokio::test]
    async fn empty_fleet_message() {
        let t = FakeTransport::new(vec![], Some("x".into()));
        let out = relay(&t, &params(), "hello").await;
        assert_eq!(out, "No running ainb sessions to relay to.");
    }

    #[tokio::test]
    async fn named_prefix_routes_to_session() {
        let t = FakeTransport::new(vec![sess("backend"), sess("frontend")], Some("ok".into()));
        let out = relay(&t, &params(), "backend: run tests").await;
        assert_eq!(out, "ok");
        let (target, msg) = t.last_send.lock().unwrap().clone().unwrap();
        assert_eq!(target, "backend");
        assert_eq!(msg, "run tests");
    }

    #[tokio::test]
    async fn bare_message_hits_conductor_default() {
        let t = FakeTransport::new(vec![sess("zebra"), sess("conductor")], Some("done".into()));
        let out = relay(&t, &params(), "status?").await;
        assert_eq!(out, "done");
        let (target, msg) = t.last_send.lock().unwrap().clone().unwrap();
        assert_eq!(target, "conductor");
        assert_eq!(msg, "status?");
    }

    #[tokio::test]
    async fn configured_default_target_used_when_no_prefix() {
        let t = FakeTransport::new(vec![sess("alpha"), sess("beta")], Some("hi".into()));
        let p = RelayParams {
            default_target: Some("beta"),
            response_timeout: Duration::from_secs(5),
        };
        let out = relay(&t, &p, "do it").await;
        assert_eq!(out, "hi");
        assert_eq!(t.last_send.lock().unwrap().clone().unwrap().0, "beta");
    }

    #[tokio::test]
    async fn named_prefix_to_missing_session_reports() {
        let t = FakeTransport::new(vec![sess("backend")], Some("x".into()));
        let out = relay(&t, &params(), "ghost: hello").await;
        // "ghost" isn't a session, so it's treated as plain text and hits the
        // default ("backend") — matching the Python known-names guard.
        assert_eq!(out, "x");
        assert_eq!(
            t.last_send.lock().unwrap().clone().unwrap().1,
            "ghost: hello"
        );
    }

    #[tokio::test]
    async fn empty_body_after_prefix_reports() {
        let t = FakeTransport::new(vec![sess("backend")], Some("x".into()));
        let out = relay(&t, &params(), "backend:   ").await;
        assert_eq!(out, "(empty message — nothing sent to backend)");
        assert!(
            t.last_send.lock().unwrap().is_none(),
            "nothing should be sent"
        );
    }

    #[tokio::test]
    async fn no_reply_in_time_reports_timeout() {
        let t = FakeTransport::new(vec![sess("backend")], None);
        let p = RelayParams {
            default_target: None,
            response_timeout: Duration::from_secs(42),
        };
        let out = relay(&t, &p, "backend: slow task").await;
        assert_eq!(
            out,
            "Sent to backend, but no reply within 42s (it may still be working)."
        );
    }
}
