// ABOUTME: Budget-breach alert delivery for `ainb fleet cost`.
//
// A breach is surfaced through the existing notifyd substrate: a valid
// `Envelope` is written to the daemon's Unix socket
// (`~/.agents-in-a-box/notify.sock`), one JSON line per write. We use the
// `Notification:budget_exceeded` raw event so it classifies as
// `AlertKind::WaitingOnUser` and passes notifyd's `is_user_facing` filter,
// landing as a row in `notifications.db`. No new alert kind is introduced —
// budget caps reuse the same delivery path as idle/permission prompts.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

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
    pub const fn session(
        session_id: String,
        cwd: Option<String>,
        cost_usd: f64,
        limit_usd: f64,
    ) -> Self {
        Self {
            scope: BudgetScope::Session,
            subject: session_id,
            cwd,
            cost_usd,
            limit_usd,
        }
    }

    pub const fn group(group: String, cost_usd: f64, limit_usd: f64) -> Self {
        Self {
            scope: BudgetScope::Group,
            subject: group,
            cwd: None,
            cost_usd,
            limit_usd,
        }
    }

    /// Stable lowercase label for the scope (`"session"` / `"group"`).
    pub const fn scope_label(&self) -> &'static str {
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

// ---------------------------------------------------------------------------
// Alert debounce.
//
// notifyd has no content dedup, so a naive `ainb fleet cost` would re-emit an
// Envelope for every standing breach on every run, accumulating duplicate
// `notifications.db` rows. We debounce by remembering, per subject, the
// highest cap-multiple we've already alerted on, persisted to
// `~/.agents-in-a-box/fleet-cost-alerts.json`. A breach only re-alerts when
// its spend crosses into a *new* multiple of the cap (1×, 2×, 3× …) — so a
// session that's been stuck just over its $5 cap alerts once at $5 and stays
// quiet until $10, while a genuinely escalating runaway still pages.
// ---------------------------------------------------------------------------

/// Filename for the persisted debounce ledger, under the notifyd base dir.
const LEDGER_FILE: &str = "fleet-cost-alerts.json";

/// Per-subject record of the highest cap-multiple already alerted on.
/// Keyed by `"<scope>:<subject>"` so a session and a group that happen to
/// share a name never alias.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AlertLedger {
    #[serde(default)]
    alerted_multiple: BTreeMap<String, u64>,
}

/// Stable ledger key for a breach: scope + subject.
fn ledger_key(breach: &BudgetBreach) -> String {
    format!("{}:{}", breach.scope_label(), breach.subject)
}

/// How many whole multiples of its cap a breach has reached
/// (`floor(cost / limit)`), clamped to ≥1. A breach by definition has
/// `cost > limit`, so this is at least 1; a non-positive limit (which never
/// produces a breach) defensively yields 1.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn breach_multiple(breach: &BudgetBreach) -> u64 {
    if breach.limit_usd <= 0.0 {
        return 1;
    }
    // A breach has cost > limit, so the ratio is finite and ≥1; the floor
    // is a small non-negative integer well within u64.
    let m = (breach.cost_usd / breach.limit_usd).floor();
    if m < 1.0 { 1 } else { m as u64 }
}

impl AlertLedger {
    /// True when this breach has crossed into a cap-multiple we have not
    /// alerted on yet (so it warrants a fresh notifyd write).
    pub fn should_alert(&self, breach: &BudgetBreach) -> bool {
        let current = breach_multiple(breach);
        match self.alerted_multiple.get(&ledger_key(breach)) {
            Some(&seen) => current > seen,
            None => true,
        }
    }

    /// Record that we've alerted on this breach's current cap-multiple.
    pub fn record(&mut self, breach: &BudgetBreach) {
        let current = breach_multiple(breach);
        let entry = self.alerted_multiple.entry(ledger_key(breach)).or_default();
        if current > *entry {
            *entry = current;
        }
    }

    /// Load the ledger from `path`, returning an empty ledger when the file
    /// is missing or unreadable (debounce must never block alerting on a
    /// corrupt/absent state file — worst case we re-alert once).
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Persist the ledger to `path`, creating parent dirs as needed.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let json = serde_json::to_string_pretty(self).context("serializing alert ledger")?;
        std::fs::write(path, json)
            .with_context(|| format!("writing alert ledger {}", path.display()))?;
        Ok(())
    }
}

/// Default debounce-ledger path (`~/.agents-in-a-box/fleet-cost-alerts.json`).
pub fn ledger_path() -> Result<PathBuf> {
    let paths = Paths::from_home().context("resolving notifyd paths")?;
    Ok(paths.base.join(LEDGER_FILE))
}

/// Emit only the breaches that have crossed a new cap-multiple since the
/// last run, then persist the updated ledger. Pure decision logic lives in
/// [`AlertLedger`]; this wires it to the default ledger path + notifyd
/// socket. Returns the breaches actually delivered.
///
/// Best-effort throughout: a notifyd write failure for one breach is logged
/// and skipped (it is NOT recorded, so it retries next run), and the ledger
/// is still saved for the breaches that did land.
pub fn emit_debounced(breaches: &[BudgetBreach]) -> Vec<&BudgetBreach> {
    let path = match ledger_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("warning: budget alert ledger path unresolved: {e}");
            // Fall back to alerting everything (no debounce) rather than
            // going silent.
            for breach in breaches {
                if let Err(e) = emit(breach) {
                    eprintln!("warning: budget alert delivery failed: {e}");
                }
            }
            return breaches.iter().collect();
        }
    };
    let mut ledger = AlertLedger::load(&path);
    let mut delivered = Vec::new();
    for breach in breaches {
        if !ledger.should_alert(breach) {
            continue;
        }
        match emit(breach) {
            Ok(()) => {
                ledger.record(breach);
                delivered.push(breach);
            }
            Err(e) => eprintln!("warning: budget alert delivery failed: {e}"),
        }
    }
    if let Err(e) = ledger.save(&path) {
        eprintln!("warning: persisting budget alert ledger failed: {e}");
    }
    delivered
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_plugin_notifyd::envelope::Envelope;

    #[test]
    fn first_breach_alerts_then_debounces_until_next_multiple() {
        let mut ledger = AlertLedger::default();
        // $6 against a $5 cap → 1× multiple. First sighting alerts.
        let b1 = BudgetBreach::session("s".into(), None, 6.0, 5.0);
        assert!(ledger.should_alert(&b1));
        ledger.record(&b1);
        // Same standing breach (still ~1×) does NOT re-alert.
        let b1_again = BudgetBreach::session("s".into(), None, 7.0, 5.0);
        assert!(!ledger.should_alert(&b1_again));
        // Crossing into 2× ($10+) alerts again.
        let b2 = BudgetBreach::session("s".into(), None, 11.0, 5.0);
        assert!(ledger.should_alert(&b2));
        ledger.record(&b2);
        // And then debounces at 2× until 3×.
        let b2_again = BudgetBreach::session("s".into(), None, 12.0, 5.0);
        assert!(!ledger.should_alert(&b2_again));
    }

    #[test]
    fn session_and_group_with_same_name_do_not_alias() {
        let mut ledger = AlertLedger::default();
        let sess = BudgetBreach::session("infra".into(), None, 6.0, 5.0);
        let group = BudgetBreach::group("infra".into(), 6.0, 5.0);
        ledger.record(&sess);
        // The group breach shares the subject string but a different scope,
        // so it must still alert.
        assert!(ledger.should_alert(&group));
    }

    #[test]
    fn ledger_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("alerts.json");
        let mut ledger = AlertLedger::default();
        let breach = BudgetBreach::group("ws".into(), 30.0, 10.0); // 3×
        ledger.record(&breach);
        ledger.save(&path).unwrap();

        let reloaded = AlertLedger::load(&path);
        assert_eq!(reloaded, ledger);
        // A fresh process at the same 3× spend stays quiet.
        assert!(!reloaded.should_alert(&BudgetBreach::group("ws".into(), 31.0, 10.0)));
        // A 4× escalation re-alerts.
        assert!(reloaded.should_alert(&BudgetBreach::group("ws".into(), 41.0, 10.0)));
    }

    #[test]
    fn missing_ledger_file_loads_empty_and_alerts() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = AlertLedger::load(&dir.path().join("does-not-exist.json"));
        assert!(ledger.should_alert(&BudgetBreach::session("x".into(), None, 9.0, 1.0)));
    }

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
            ingest_interval: std::time::Duration::from_millis(20),
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
