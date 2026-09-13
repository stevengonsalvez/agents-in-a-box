// ABOUTME: Channel-agnostic relay core shared by the Telegram + Slack channels.
//
// Ported from the Python `bridge._relay`: resolve the target session from the
// raw message (conductor-prefix routing, degrade-to-default), send it, and
// return a single user-facing reply string. Both channels call exactly this —
// the ONE relay/routing core the goal requires — so their behaviour can never
// diverge. The channel layer owns only transport-specific concerns (auth,
// mention-gating, markdown rendering, message splitting).

use std::time::Duration;

use super::routing::{
    TargetSession, best_match, default_target, parse_target_prefix, resolve_target,
};

/// The transport seam the relay drives. The real implementation shells out to
/// `ainb`/tmux (`transport.rs`); tests substitute an in-memory fake so the
/// routing + degrade logic is verified without a live fleet.
///
/// Uses native `async fn` in traits (stable since Rust 1.75). The methods spell
/// out `impl Future<…> + Send` explicitly (rather than bare `async fn`) so the
/// returned futures are guaranteed `Send`: the Discord channel runs the relay in
/// its own `tokio::spawn` task (so a slow agent reply can't starve the gateway
/// heartbeat), and `tokio::spawn` requires a `Send` future. The relay is still
/// only ever called through a generic `&T`, never a `dyn` object.
pub trait FleetTransport: Send + Sync {
    /// Currently-running relay targets.
    fn discover(&self) -> impl std::future::Future<Output = Vec<TargetSession>> + Send;
    /// Send `text` to `session` and capture its end-of-turn reply (or `None`
    /// on send failure / timeout).
    fn send_and_capture(
        &self,
        session: &TargetSession,
        text: &str,
        timeout: Duration,
    ) -> impl std::future::Future<Output = Option<String>> + Send;
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

    let (parsed_name, message) = parse_target_prefix(raw_text, &sessions);

    let target = if let Some(name) = parsed_name.as_deref() {
        // Explicit `name:` prefix that already matched a real session.
        let Some(target) = resolve_target(Some(name), &sessions) else {
            return format!("No running session named {name:?}.");
        };
        target
    } else if let Some(default) = params.default_target {
        // A default target IS configured. It MUST resolve to a running session
        // (by run-name or workspace name). If it doesn't, refuse to relay —
        // returning a clear error rather than silently falling through to the
        // conductor/alphabetical default, which is how an un-prefixed message
        // ended up in the user's active orchestrator session.
        let Some(target) = best_match(default, &sessions) else {
            return format!("No running session matches default target {default:?}.");
        };
        target
    } else {
        // No prefix AND no configured default — only here may we use the
        // conductor-first / alphabetical default.
        let Some(target) = default_target(&sessions) else {
            return "No target session available.".to_string();
        };
        target
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

    /// A session built the way live discovery builds it: a run-name distinct
    /// from the workspace/repo folder name. Exercising the relay through these
    /// is what closes the gap the old unit tests left (they only ever used
    /// `sess`, where run-name == workspace, hiding the mis-route).
    fn sess_ws(run_name: &str, workspace: &str) -> TargetSession {
        TargetSession::with_workspace(
            run_name,
            workspace,
            format!("tmux_{run_name}"),
            format!("/cwd/{workspace}"),
            format!("id-{run_name}"),
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

    // --- Discovery->routing regression tests (the live mis-route) ------------
    //
    // These use `sess_ws` (run-name != workspace), reproducing what
    // `transport::discover()` actually builds. The pre-fix relay matched
    // `default_target` against the workspace name only, so a `default_target`
    // set to a run-name fell through to the conductor/alphabetical default and
    // landed in the wrong (often the user's active orchestrator) session.

    /// (a) default_target set to a session's `ainb run --name` routes THERE,
    /// even though the session's workspace folder name is something else.
    #[tokio::test]
    async fn default_target_matches_run_name_from_discovery() {
        // Orchestrator session is alphabetically first by run-name AND workspace.
        let orchestrator = sess_ws("agents-in-a-box", "agents-in-a-box");
        let bridgetest = sess_ws("bridgetest", "tmp");
        let t = FakeTransport::new(vec![orchestrator, bridgetest], Some("routed".into()));
        let p = RelayParams {
            default_target: Some("bridgetest"),
            response_timeout: Duration::from_secs(5),
        };
        let out = relay(&t, &p, "hello there").await;
        assert_eq!(out, "routed");
        // Crucial: it went to bridgetest, NOT the alphabetical orchestrator.
        assert_eq!(t.last_send.lock().unwrap().clone().unwrap().0, "bridgetest");
    }

    /// (b) default_target SET but matching NO running session -> hard error, and
    /// nothing is sent. NEVER the alphabetical/conductor fallback.
    #[tokio::test]
    async fn unmatched_default_target_errors_instead_of_falling_back() {
        let orchestrator = sess_ws("agents-in-a-box", "agents-in-a-box");
        let other = sess_ws("integration-port", "integration-port");
        let t = FakeTransport::new(vec![orchestrator, other], Some("should-not-send".into()));
        let p = RelayParams {
            default_target: Some("bridgetest"), // not running
            response_timeout: Duration::from_secs(5),
        };
        let out = relay(&t, &p, "hello there").await;
        assert_eq!(
            out,
            "No running session matches default target \"bridgetest\"."
        );
        assert!(
            t.last_send.lock().unwrap().is_none(),
            "must NOT relay to any session when the configured default is unmatched"
        );
    }

    /// (c) no default_target AND no prefix -> the conductor/alphabetical default
    /// still applies, unchanged. This is the only path that may fall back.
    #[tokio::test]
    async fn no_default_no_prefix_still_uses_conductor_default() {
        let zebra = sess_ws("zebra", "repo-z");
        let conductor = sess_ws("conductor", "repo-c");
        let t = FakeTransport::new(vec![zebra, conductor], Some("done".into()));
        let p = RelayParams {
            default_target: None,
            response_timeout: Duration::from_secs(5),
        };
        let out = relay(&t, &p, "status?").await;
        assert_eq!(out, "done");
        assert_eq!(t.last_send.lock().unwrap().clone().unwrap().0, "conductor");
    }

    /// default_target may also be addressed by the workspace name (fallback
    /// alias), so the original pre-fix contract still holds.
    #[tokio::test]
    async fn default_target_matches_workspace_name_fallback() {
        let bridgetest = sess_ws("bridgetest", "tmp");
        let t = FakeTransport::new(vec![sess("other"), bridgetest], Some("ok".into()));
        let p = RelayParams {
            default_target: Some("tmp"), // the workspace, not the run-name
            response_timeout: Duration::from_secs(5),
        };
        let out = relay(&t, &p, "go").await;
        assert_eq!(out, "ok");
        assert_eq!(t.last_send.lock().unwrap().clone().unwrap().0, "bridgetest");
    }
}
