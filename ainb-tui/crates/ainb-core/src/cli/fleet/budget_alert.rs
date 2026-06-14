// ABOUTME: Budget-breach alert delivery for `ainb fleet cost`.
//
// A breach is surfaced through the existing notifyd substrate: a valid
// `Envelope` is written to the daemon's Unix socket
// (`~/.agents-in-a-box/notify.sock`), one JSON line per write. We use the
// `Notification:budget_exceeded` raw event so it classifies as
// `AlertKind::WaitingOnUser` and passes notifyd's `is_user_facing` filter,
// landing as a row in `notifications.db`. No new alert kind is introduced —
// budget caps reuse the same delivery path as idle/permission prompts.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use ainb_plugin_notifyd::envelope::PROTOCOL_VERSION;
use ainb_plugin_notifyd::paths::Paths;

/// Raw event used for budget alerts. The `Notification` head classifies as
/// [`ainb_plugin_notifyd::AlertKind::WaitingOnUser`]; the `:budget_exceeded`
/// matcher suffix is preserved verbatim through notifyd for the inbox view.
const BUDGET_RAW_EVENT: &str = "Notification:budget_exceeded";

/// What kind of budget ceiling was crossed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BudgetScope {
    Session,
    Group,
}

/// A single budget breach: a session or group whose spend crossed its
/// configured USD ceiling. Serialized into the `fleet cost` JSON report and
/// turned into a notifyd `Envelope`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BudgetBreach {
    pub scope: BudgetScope,
    /// The session id (scope = session) or group name (scope = group).
    pub subject: String,
    /// Working directory of the offending session, when known. Used as the
    /// notifyd `cwd` so the alert correlates to the fleet session by the
    /// canonical `Session.workspace_path == Notification.cwd` join.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub cost_usd: f64,
    pub limit_usd: f64,
}

impl BudgetBreach {
    pub fn session(session_id: String, cwd: Option<String>, cost_usd: f64, limit_usd: f64) -> Self {
        Self {
            scope: BudgetScope::Session,
            subject: session_id,
            cwd,
            cost_usd,
            limit_usd,
        }
    }

    pub fn group(group: String, cost_usd: f64, limit_usd: f64) -> Self {
        Self {
            scope: BudgetScope::Group,
            subject: group,
            cwd: None,
            cost_usd,
            limit_usd,
        }
    }

    /// Stable lowercase label for the scope (`"session"` / `"group"`).
    pub fn scope_label(&self) -> &'static str {
        match self.scope {
            BudgetScope::Session => "session",
            BudgetScope::Group => "group",
        }
    }

    /// Human-readable one-liner used as the notification body.
    fn message(&self) -> String {
        format!(
            "{} {} spent ${:.2}, over its ${:.2} budget cap",
            self.scope_label(),
            self.subject,
            self.cost_usd,
            self.limit_usd
        )
    }

    /// Render this breach as a notifyd `Envelope` JSON object.
    fn envelope_json(&self) -> serde_json::Value {
        let cwd = self.cwd.clone().unwrap_or_default();
        let project = if cwd.is_empty() {
            self.subject.clone()
        } else {
            Path::new(&cwd)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&self.subject)
                .to_string()
        };
        serde_json::json!({
            "protocol_version": PROTOCOL_VERSION,
            "agent": "ainb",
            "raw_event": BUDGET_RAW_EVENT,
            "session_id": match self.scope {
                BudgetScope::Session => self.subject.clone(),
                BudgetScope::Group => String::new(),
            },
            "cwd": cwd,
            "project": project,
            "ts": chrono::Utc::now().timestamp_millis(),
            "payload": {
                "kind": "budget_exceeded",
                "scope": self.scope_label(),
                "subject": self.subject,
                "cost_usd": self.cost_usd,
                "limit_usd": self.limit_usd,
                "message": self.message(),
            },
        })
    }
}

/// Deliver a breach alert to the default notifyd socket
/// (`~/.agents-in-a-box/notify.sock`).
pub fn emit(breach: &BudgetBreach) -> Result<()> {
    let paths = Paths::from_home().context("resolving notifyd paths")?;
    emit_to(&paths.socket, breach)
}

/// Deliver a breach alert to a specific notifyd socket path. Pulled out so
/// tests can target a daemon running on a temp socket without touching the
/// user's real `~/.agents-in-a-box`.
pub fn emit_to(socket: &Path, breach: &BudgetBreach) -> Result<()> {
    let mut line = serde_json::to_string(&breach.envelope_json())
        .context("serializing budget alert envelope")?;
    line.push('\n');
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connecting to notifyd socket {}", socket.display()))?;
    stream
        .write_all(line.as_bytes())
        .context("writing budget alert envelope to notifyd socket")?;
    stream.flush().ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_plugin_notifyd::envelope::Envelope;

    #[test]
    fn breach_envelope_validates_and_classifies_as_user_facing() {
        let breach = BudgetBreach::session("sess-x".into(), Some("/work/x".into()), 7.5, 5.0);
        let json = breach.envelope_json();
        let bytes = serde_json::to_vec(&json).unwrap();
        // The envelope must parse + validate under notifyd's contract.
        let env = Envelope::from_bytes(&bytes).expect("valid envelope");
        assert_eq!(env.raw_event, "Notification:budget_exceeded");
        assert_eq!(env.cwd, "/work/x");
        assert_eq!(env.session_id, "sess-x");
        // And it must survive notifyd's user-facing filter (else the daemon
        // would silently drop it).
        assert!(ainb_plugin_notifyd::osnotify::is_user_facing(&env));
        assert_eq!(
            ainb_plugin_notifyd::classify_attention(&env.raw_event),
            Some(ainb_plugin_notifyd::AlertKind::WaitingOnUser)
        );
    }

    #[test]
    fn group_breach_has_empty_session_id() {
        let breach = BudgetBreach::group("ws-infra".into(), 30.0, 25.0);
        let json = breach.envelope_json();
        assert_eq!(json["session_id"], "");
        assert_eq!(json["project"], "ws-infra");
    }

    /// End-to-end proof of success-criterion #2: crossing a low budget
    /// threshold delivers a notifyd row. Spins the real daemon on a temp
    /// socket, emits a session breach, and asserts the row lands in
    /// `notifications.db` with the budget raw event.
    #[tokio::test]
    async fn budget_breach_lands_in_notifications_db() {
        use ainb_plugin_notifyd::{Paths, RetentionPolicy, RunConfig, Store, run_daemon};
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path().join(".agents-in-a-box"));
        paths.ensure_base().unwrap();

        let config = RunConfig {
            paths: paths.clone(),
            // 0/0 disables pruning so the single test row survives.
            retention: RetentionPolicy {
                retention_days: 0,
                max_rows: 0,
            },
            os_notifications: false,
        };
        let socket = paths.socket.clone();
        let daemon = tokio::spawn(async move { run_daemon(config).await });

        // Wait for the daemon to bind its socket.
        for _ in 0..200 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(socket.exists(), "daemon socket never appeared");

        // The breach: a session that spent $9 against a $1 cap.
        let breach = BudgetBreach::session("sess-over".into(), Some("/work/over".into()), 9.0, 1.0);
        let socket_for_blocking = socket.clone();
        tokio::task::spawn_blocking(move || emit_to(&socket_for_blocking, &breach))
            .await
            .unwrap()
            .expect("emit budget alert");

        // Give the daemon a tick to drain the write.
        tokio::time::sleep(Duration::from_millis(250)).await;

        let store = Store::open(&paths.db).unwrap();
        let rows = store.list(false, None, None, 100).unwrap();
        assert_eq!(
            rows.len(),
            1,
            "expected exactly one alert row, got {rows:?}"
        );
        let row = &rows[0];
        assert_eq!(row.agent, "ainb");
        assert_eq!(row.raw_event, "Notification:budget_exceeded");
        assert_eq!(row.session_id, "sess-over");
        assert_eq!(row.cwd, "/work/over");
        assert!(
            row.payload_json.contains("budget_exceeded"),
            "payload missing budget marker: {}",
            row.payload_json
        );

        daemon.abort();
    }
}
