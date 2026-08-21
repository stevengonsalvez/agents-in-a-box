// ABOUTME: Tier-A Claude probe — first-party session state from
// `~/.claude/sessions/<pid>.json`, the highest-quality evidence tier.
//
// Claude Code (verified on 2.1.220 through 2.1.227) writes one probe file per
// live session: `{pid, sessionId, cwd, status, waitingFor, statusUpdatedAt,
// procStart, ...}` with `status` ∈ busy | idle | waiting | shell. It is the
// only source in the fleet that states BUSY affirmatively — every other tier
// can only infer "not needing attention" from absence — and it reports
// `waiting` the instant a session blocks, with no idle-minutes heuristic.
//
// It is also an UNDOCUMENTED internal file, so it is a preferred source and
// never the only one: any gate failure here returns `None`/`Fallback` and the
// session flows to the hook tier and then the pane scan, exactly as today.
//
// Everything that decides is pure: parsing, the liveness gate, and the
// status→[`Resolution`] mapping all take their inputs as data. The only I/O is
// [`load_dir`], a thin directory listing.

use std::path::Path;

use serde::Deserialize;

use crate::fleet::read::needs::make_row;
use crate::fleet::read::{
    AskUserQuestionData, IdleContext, NeedsContext, Resolution, RouteHint, WaitContext,
};
use crate::fleet::types::Session;

/// Provenance stamp for probe-sourced rows, alongside the materializer's
/// `"hook"` and the fallback's `"tmux"`.
pub const SOURCE_PROBE: &str = "probe";

/// One parsed `~/.claude/sessions/<pid>.json` probe.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeProbe {
    /// The Claude Code process. Liveness is gated on this pid still running.
    pub pid: u32,
    /// Claude's own session id (not the ainb session id).
    #[serde(default)]
    pub session_id: String,
    /// Working directory — the correlation key to an ainb session.
    #[serde(default)]
    pub cwd: String,
    /// `busy` | `idle` | `waiting` | `shell`; anything else abstains.
    #[serde(default)]
    pub status: String,
    /// Human-readable reason when `status == "waiting"` ("input needed").
    #[serde(default)]
    pub waiting_for: Option<String>,
    /// Epoch-ms of the last status flip. Ages an `idle` into an IDLE row.
    #[serde(default)]
    pub status_updated_at: i64,
    /// `ps lstart`-style start time of `pid`. Defeats pid recycling: a
    /// recycled pid is alive but reports a different start time.
    #[serde(default)]
    pub proc_start: String,
}

/// Parse one probe file's contents. Corrupt/foreign JSON is `None`, never an
/// error: this sits on a read path that must not fail, and an unreadable probe
/// simply means the session falls through to the next tier.
#[must_use]
pub fn parse_probe(json: &str) -> Option<ClaudeProbe> {
    serde_json::from_str(json).ok()
}

/// What the host observed about the probe's pid, gathered by the caller (the
/// one impure step) so the gate itself stays pure and table-testable.
#[derive(Debug, Clone, Default)]
pub struct PidObservation {
    /// `kill -0`-style liveness of the pid.
    pub alive: bool,
    /// The running process's start time in the SAME `ps lstart` format the
    /// probe records, or `None` when the process (or its start time) could not
    /// be read.
    pub proc_start: Option<String>,
}

/// The liveness gate: trust a probe only when its pid is alive AND the running
/// process's start time matches the recorded one exactly.
///
/// Both checks are required. Alive-only trusts a recycled pid; a missing or
/// mismatched start time reads as NOT live rather than "probably fine",
/// because the failure mode of wrongly trusting a probe is a session silently
/// classified from another process's state, while the failure mode of wrongly
/// distrusting one is a fall-through to the tiers that run today anyway.
#[must_use]
pub fn probe_is_live(probe: &ClaudeProbe, observed: &PidObservation) -> bool {
    observed.alive
        && !probe.proc_start.is_empty()
        && observed.proc_start.as_deref() == Some(probe.proc_start.as_str())
}

/// Map a LIVE probe to the three-way [`Resolution`] the resolve loop already
/// speaks, or `None` when this tier abstains (unknown status, un-ageable
/// idle) and the session should flow to the hook tier untouched.
///
/// The mapping preserves today's ATC semantics exactly — it upgrades the
/// EVIDENCE, not the policy:
///
/// - `busy` / `shell` → [`Resolution::Healthy`]: the first affirmative
///   running state the fleet has ever had. No needs row, no pane scan, and an
///   in-flight turn can no longer be mistaken for a stuck session.
/// - `idle` → IDLE row once it has aged past `idle_threshold_min` (the same
///   threshold the transcript path uses), else `Healthy`. A probe with no
///   usable `statusUpdatedAt` abstains — age cannot be proven.
/// - `waiting` → an ASK row when the caller extracted the open question from
///   the transcript (`ask`), else a WAIT row carrying `waitingFor`. The probe
///   knows THAT the session blocked the moment it happened; the transcript
///   still knows WHAT is being asked.
#[must_use]
pub fn resolve_probe(
    probe: &ClaudeProbe,
    session: Session,
    ask: Option<AskUserQuestionData>,
    idle_threshold_min: i64,
    now_ms: i64,
) -> Option<Resolution> {
    let route_hint = if session.tmux_session.is_some() {
        RouteHint::Tmux
    } else {
        RouteHint::Broker
    };
    let stamped = |session: Session, ctx: NeedsContext| {
        let mut row = make_row(session, ctx, route_hint);
        row.source = Some(SOURCE_PROBE.to_string());
        Resolution::Hook(Box::new(row))
    };

    match probe.status.as_str() {
        "busy" | "shell" => Some(Resolution::Healthy),
        "idle" => {
            if probe.status_updated_at <= 0 {
                return None; // cannot prove age — abstain, let lower tiers age it
            }
            let idle_minutes = now_ms.saturating_sub(probe.status_updated_at) / 60_000;
            if idle_minutes >= idle_threshold_min {
                Some(stamped(
                    session,
                    NeedsContext::Idle(IdleContext {
                        idle_minutes,
                        last_assistant_text: None,
                    }),
                ))
            } else {
                Some(Resolution::Healthy)
            }
        }
        "waiting" => {
            if let Some(aq) = ask {
                Some(stamped(session, NeedsContext::Ask(aq)))
            } else {
                Some(stamped(
                    session,
                    NeedsContext::Wait(WaitContext {
                        marker: "waitingFor:".to_string(),
                        text: probe
                            .waiting_for
                            .clone()
                            .unwrap_or_else(|| "input needed".to_string()),
                    }),
                ))
            }
        }
        _ => None,
    }
}

/// Load every parseable probe in `dir` (`~/.claude/sessions`). Missing dir or
/// unreadable entries yield an empty/partial set — this tier degrades, never
/// errors. Liveness is NOT checked here; the caller gates each candidate with
/// [`probe_is_live`] so the pid observations happen once, next to the ps call.
#[must_use]
pub fn load_dir(dir: &Path) -> Vec<ClaudeProbe> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut probes: Vec<ClaudeProbe> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|s| parse_probe(&s))
        .collect();
    // Deterministic order for stable downstream correlation when two probes
    // share a cwd (newest status flip wins in the P3 index).
    probes.sort_by_key(|p| (p.cwd.clone(), std::cmp::Reverse(p.status_updated_at)));
    probes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::types::{Session, SessionSource};

    /// A real probe captured from Claude Code 2.1.224 on 2026-08-12 (values
    /// abridged) — the parser must handle the genuine shape, unknown fields
    /// included.
    const REAL_PROBE: &str = r#"{"pid":29266,"sessionId":"5d2a6f87-d870-4bbf-b81a-506c153ec7ec",
        "cwd":"/Users/x/d/git","startedAt":1786098934524,"procStart":"Fri Aug  7 10:35:33 2026",
        "version":"2.1.224","peerProtocol":1,"kind":"bg","entrypoint":"cli",
        "messagingSocketPath":"/tmp/cc-socks/29266.sock","name":"gog cli verification",
        "agent":"claude","jobId":"5d2a6f87","status":"waiting","updatedAt":1786106681993,
        "statusUpdatedAt":1786106681993,"bridgeSessionId":null,"waitingFor":"input needed"}"#;

    fn session(tmux: Option<&str>) -> Session {
        Session {
            id: "s1".into(),
            cwd: "/w/s1".into(),
            pid: None,
            git_root: None,
            tmux_session: tmux.map(Into::into),
            workspace_name: None,
            worktree_path: None,
            peer_id: None,
            bg_job_id: None,
            transcript_path: None,
            sources: vec![SessionSource::Ainb],
            summary: None,
            last_seen_ms: None,
        }
    }

    fn probe(status: &str, updated_at: i64) -> ClaudeProbe {
        ClaudeProbe {
            pid: 100,
            session_id: "cs1".into(),
            cwd: "/w/s1".into(),
            status: status.into(),
            waiting_for: None,
            status_updated_at: updated_at,
            proc_start: "Fri Aug  7 10:35:33 2026".into(),
        }
    }

    fn live() -> PidObservation {
        PidObservation {
            alive: true,
            proc_start: Some("Fri Aug  7 10:35:33 2026".into()),
        }
    }

    #[test]
    fn parses_the_real_probe_shape_with_unknown_fields() {
        let p = parse_probe(REAL_PROBE).expect("real probe parses");
        assert_eq!(p.pid, 29266);
        assert_eq!(p.status, "waiting");
        assert_eq!(p.waiting_for.as_deref(), Some("input needed"));
        assert_eq!(p.proc_start, "Fri Aug  7 10:35:33 2026");
        assert_eq!(p.status_updated_at, 1786106681993);
    }

    #[test]
    fn corrupt_and_foreign_json_abstain_instead_of_erroring() {
        assert!(parse_probe("not json").is_none());
        assert!(parse_probe(r#"{"unrelated":true}"#).is_none()); // no pid
    }

    #[test]
    fn liveness_requires_alive_pid_and_exact_proc_start() {
        let p = probe("busy", 1);
        assert!(probe_is_live(&p, &live()));
        // Dead pid.
        assert!(!probe_is_live(
            &p,
            &PidObservation {
                alive: false,
                ..live()
            }
        ));
        // Recycled pid: alive but a different start time.
        assert!(!probe_is_live(
            &p,
            &PidObservation {
                alive: true,
                proc_start: Some("Sat Aug  8 09:00:00 2026".into())
            }
        ));
        // Unreadable start time reads as NOT live, never "probably fine".
        assert!(!probe_is_live(
            &p,
            &PidObservation {
                alive: true,
                proc_start: None
            }
        ));
        // A probe with no recorded start can never pass the gate.
        let mut blank = p;
        blank.proc_start = String::new();
        assert!(!probe_is_live(
            &blank,
            &PidObservation {
                alive: true,
                proc_start: Some(String::new())
            }
        ));
    }

    #[test]
    fn busy_and_shell_are_the_affirmative_healthy_states() {
        for s in ["busy", "shell"] {
            assert!(
                matches!(
                    resolve_probe(&probe(s, 1), session(None), None, 5, 60_000),
                    Some(Resolution::Healthy)
                ),
                "{s} must short-circuit as Healthy"
            );
        }
    }

    #[test]
    fn young_idle_is_healthy_old_idle_is_an_idle_row() {
        let now = 10 * 60_000;
        // 4 minutes old, threshold 5 — healthy, no row, no scan.
        assert!(matches!(
            resolve_probe(
                &probe("idle", now - 4 * 60_000),
                session(None),
                None,
                5,
                now
            ),
            Some(Resolution::Healthy)
        ));
        // 7 minutes old — an IDLE row stamped with the probe source.
        let Some(Resolution::Hook(row)) = resolve_probe(
            &probe("idle", now - 7 * 60_000),
            session(None),
            None,
            5,
            now,
        ) else {
            panic!("aged idle must produce a row");
        };
        assert_eq!(row.source.as_deref(), Some(SOURCE_PROBE));
        let NeedsContext::Idle(idle) = &row.context else {
            panic!("wrong context: {:?}", row.context);
        };
        assert_eq!(idle.idle_minutes, 7);
    }

    #[test]
    fn idle_without_a_usable_timestamp_abstains() {
        assert!(resolve_probe(&probe("idle", 0), session(None), None, 5, 60_000).is_none());
    }

    #[test]
    fn waiting_prefers_the_transcript_question_else_carries_waiting_for() {
        let mut p = probe("waiting", 1);
        p.waiting_for = Some("input needed".into());

        // With the transcript's open question: a full ASK row.
        let aq = AskUserQuestionData {
            question: "merge or rebase?".into(),
            header: None,
            options: Vec::new(),
            multi_select: false,
        };
        let Some(Resolution::Hook(row)) =
            resolve_probe(&p, session(Some("t1")), Some(aq), 5, 60_000)
        else {
            panic!("waiting+ask must produce a row");
        };
        assert!(matches!(&row.context, NeedsContext::Ask(a) if a.question == "merge or rebase?"));
        assert_eq!(row.source.as_deref(), Some(SOURCE_PROBE));
        assert!(matches!(row.route_hint, RouteHint::Tmux));

        // Without it: a WAIT row carrying the probe's own reason.
        let Some(Resolution::Hook(row)) = resolve_probe(&p, session(None), None, 5, 60_000) else {
            panic!("waiting without ask must still produce a row");
        };
        let NeedsContext::Wait(w) = &row.context else {
            panic!("wrong context: {:?}", row.context);
        };
        assert_eq!(w.text, "input needed");
        assert!(matches!(row.route_hint, RouteHint::Broker));
    }

    #[test]
    fn unknown_status_abstains_for_the_lower_tiers() {
        assert!(resolve_probe(&probe("compacting", 1), session(None), None, 5, 60_000).is_none());
    }

    #[test]
    fn load_dir_skips_unreadable_entries_and_missing_dirs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("29266.json"), REAL_PROBE).unwrap();
        std::fs::write(dir.path().join("bad.json"), "not json").unwrap();
        std::fs::write(dir.path().join("ignore.txt"), "nope").unwrap();
        let probes = load_dir(dir.path());
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].pid, 29266);
        assert!(load_dir(&dir.path().join("absent")).is_empty());
    }
}
