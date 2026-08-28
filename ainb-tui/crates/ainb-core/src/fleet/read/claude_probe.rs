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

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::fleet::read::needs::make_row;
use crate::fleet::read::{
    AskUserQuestionData, IdleContext, NeedsContext, Resolution, RouteHint, WaitContext,
};
use crate::fleet::types::{Session, SessionSource};

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
    /// Claude's own display name for the session, when it has one.
    #[serde(default)]
    pub name: Option<String>,
    /// When the SESSION began, epoch-ms. Deliberately NOT the liveness gate:
    /// measured against three live sessions it runs up to 353s later than the
    /// process itself, because a session starts after the process hosting it.
    #[serde(default)]
    pub started_at: i64,
    /// The PROCESS start time as a UTC asctime (`"Fri Aug  7 10:35:33 2026"`).
    /// This is the pid-recycling gate: parsed as UTC it matches `ps lstart`
    /// (parsed as local) to the second on every session measured.
    ///
    /// Compared as EPOCHS, never as strings — the two formats differ in both
    /// zone and field order (`"Fri Aug  7"` vs `"Fri  7 Aug"`), so a string
    /// compare fails on every host that is not UTC.
    #[serde(default)]
    pub proc_start: String,
}

/// Parse one probe file's contents. Corrupt/foreign JSON is `None`, never an
/// error: this sits on a read path that must not fail, and an unreadable probe
/// simply means the session falls through to the next tier.
#[must_use]
pub fn parse_probe(json: &str) -> Option<ClaudeProbe> {
    // Take the FIRST JSON value and ignore anything after it, rather than
    // `from_str`, which rejects trailing bytes outright. A real probe on the
    // host this was written against ends `...}}` — one brace too many — and a
    // strict parse threw away a whole session's state over a stray byte the
    // record itself did not need. The leading object is complete and
    // self-describing, so honouring it is strictly better than abstaining.
    //
    // Still `Option`: a genuinely truncated or foreign file yields `None` and
    // the session falls through to the tiers below.
    serde_json::Deserializer::from_str(json)
        .into_iter::<ClaudeProbe>()
        .next()
        .and_then(Result::ok)
}

/// What the host observed about the probe's pid, gathered by the caller (the
/// one impure step) so the gate itself stays pure and table-testable.
#[derive(Debug, Clone, Default)]
pub struct PidObservation {
    /// `kill -0`-style liveness of the pid.
    pub alive: bool,
    /// The running process's start instant as epoch-ms, or `None` when the
    /// process (or its start time) could not be read.
    pub started_epoch_ms: Option<i64>,
}

/// Tolerance when comparing the recorded process start against the observed
/// one. Both sides are whole seconds, so this only absorbs rounding; a recycled
/// pid misses by minutes at least.
pub const START_MATCH_TOLERANCE_MS: i64 = 2_000;

/// The liveness gate: trust a probe only when its pid is alive AND the running
/// process started within [`START_MATCH_TOLERANCE_MS`] of the recorded
/// instant.
///
/// Both checks are required. Alive-only trusts a recycled pid; a missing or
/// mismatched start time reads as NOT live rather than "probably fine",
/// because the failure mode of wrongly trusting a probe is a session silently
/// classified from another process's state, while the failure mode of wrongly
/// distrusting one is a fall-through to the tiers that run today anyway.
#[must_use]
pub fn probe_is_live(probe: &ClaudeProbe, observed: &PidObservation) -> bool {
    let Some(recorded) = parse_proc_start_utc(&probe.proc_start) else {
        return false;
    };
    observed.alive
        && observed
            .started_epoch_ms
            .is_some_and(|obs| (obs - recorded).abs() <= START_MATCH_TOLERANCE_MS)
}

/// Observe `pid` on the host: one `LC_ALL=C ps` call answers both liveness
/// (empty output = dead) and the start instant (`lstart`, parsed as LOCAL time
/// — `ps` prints the host zone — then converted to epoch-ms).
#[must_use]
pub fn observe_pid(pid: u32) -> PidObservation {
    let out = std::process::Command::new("ps")
        .env("LC_ALL", "C")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output();
    let Ok(out) = out else {
        return PidObservation::default();
    };
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || text.is_empty() {
        return PidObservation::default();
    }
    PidObservation {
        alive: true,
        started_epoch_ms: parse_lstart_local(&text),
    }
}

/// Parse Claude's `procStart` asctime as UTC into epoch-ms.
///
/// Same layout `ps` prints, different zone: Claude records UTC, `ps` prints
/// host-local. Both are normalised to epoch here so the gate compares instants
/// rather than text.
#[must_use]
fn parse_proc_start_utc(s: &str) -> Option<i64> {
    let naive = chrono::NaiveDateTime::parse_from_str(s.trim(), "%a %b %e %H:%M:%S %Y").ok()?;
    Some(naive.and_utc().timestamp_millis())
}

/// Parse a `LC_ALL=C ps -o lstart` value ("Thu Aug 20 19:53:18 2026") as host
/// LOCAL time into epoch-ms. `%e` absorbs the padded day asctime uses.
fn parse_lstart_local(s: &str) -> Option<i64> {
    use chrono::TimeZone;
    let naive = chrono::NaiveDateTime::parse_from_str(s.trim(), "%a %b %e %H:%M:%S %Y").ok()?;
    // On a DST fold prefer the earliest mapping; ambiguity within the gate's
    // 2s tolerance is not reachable in practice.
    chrono::Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|dt| dt.timestamp_millis())
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

    // A probe older than this is not evidence about NOW. Claude rewrites the
    // file on every status flip, so a stamp this old means the writer stopped:
    // the process wedged, or was SIGKILLed without cleanup. The pid stays alive
    // in both cases, so the liveness gate cannot see it.
    //
    // Only `busy`/`shell` need this. They are the states that SUPPRESS a
    // session, so a frozen one would delete a live session from `fleet needs`
    // permanently. `idle` and `waiting` produce rows that carry their own age,
    // where staleness is visible rather than silent.
    if matches!(probe.status.as_str(), "busy" | "shell") {
        let fresh = blocked_for(probe, now_ms).is_some_and(|mins| mins < HEALTHY_MAX_AGE_MIN);
        if !fresh {
            return None; // abstain — let the tiers that can actually look decide
        }
    }

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
                return Some(stamped(session, NeedsContext::Ask(aq)));
            }
            let reason = probe.waiting_for.clone().unwrap_or_else(|| "input needed".to_string());
            // Carry HOW LONG it has been blocked, not just that it is.
            //
            // Deliberately NOT a staleness cutoff. Dropping an aged `waiting`
            // row would discard exactly the case this tier exists to catch: a
            // session on this machine has been blocked for 18 days and never
            // appeared in `fleet needs` once. A wedged-but-alive Claude is
            // indistinguishable from a genuinely patient one, and the pid gate
            // already removes the crashed case, so the honest move is to
            // surface the age and let the reader judge rather than silently
            // suppress a real request for input.
            let text = match blocked_for(probe, now_ms) {
                Some(mins) => format!("{reason} (blocked {})", humanize_minutes(mins)),
                None => reason,
            };
            Some(stamped(
                session,
                NeedsContext::Wait(WaitContext {
                    marker: "waitingFor:".to_string(),
                    text,
                }),
            ))
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

/// How stale a `busy`/`shell` probe may be and still suppress a session.
///
/// Generous on purpose: a long tool call legitimately holds `busy` without a
/// rewrite, and wrongly abstaining merely costs a pane scan, while wrongly
/// trusting a frozen probe hides a stuck session indefinitely.
pub const HEALTHY_MAX_AGE_MIN: i64 = 30;

/// Minutes a probe has held its current status, or `None` when the stamp is
/// unusable (absent, or in the future — a clock skew we will not reason about).
#[must_use]
fn blocked_for(probe: &ClaudeProbe, now_ms: i64) -> Option<i64> {
    if probe.status_updated_at <= 0 || probe.status_updated_at > now_ms {
        return None;
    }
    Some((now_ms - probe.status_updated_at) / 60_000)
}

/// Compact human duration for a nudge body: `12m`, `3h`, `18d`.
#[must_use]
#[allow(clippy::missing_const_for_fn)] // formats, cannot be const
fn humanize_minutes(mins: i64) -> String {
    if mins < 60 {
        format!("{mins}m")
    } else if mins < 60 * 24 {
        format!("{}h", mins / 60)
    } else {
        format!("{}d", mins / (60 * 24))
    }
}

/// Tier-A index: every LIVE probe on this host, keyed by working directory.
///
/// Built once per `fleet needs` run so the pid observations (one `ps` each)
/// happen a bounded number of times rather than per session. A probe whose pid
/// fails [`probe_is_live`] never enters the index, so a stale file from a
/// long-dead session cannot answer for a session running in the same cwd today.
#[derive(Debug, Default)]
pub struct ProbeIndex {
    /// The freshest live probe per cwd, for RESOLVING a session.
    by_cwd: HashMap<String, ClaudeProbe>,
    /// Every live probe, for DISCOVERY. Two Claude sessions can share a cwd,
    /// and collapsing them here would delete the loser from the fleet
    /// entirely — including a blocked one losing to a busy sibling.
    all: Vec<ClaudeProbe>,
    /// By Claude's own session id — the EXACT correlation, used ahead of cwd.
    ///
    /// cwd alone is ambiguous the moment two sessions share a directory, and
    /// the cwd map keeps only the freshest: a session blocked on a question
    /// would be masked by an idle sibling that merely flipped status more
    /// recently, which is precisely the case this tier exists to surface.
    by_session: HashMap<String, ClaudeProbe>,
}

impl ProbeIndex {
    /// Load and gate every probe under `dir`.
    ///
    /// When two live probes share a cwd (two Claude sessions in one directory)
    /// the most recently updated wins: it is the one whose status was last
    /// observed to change, so it is the freshest claim about that directory.
    #[must_use]
    pub fn load_from(dir: &Path) -> Self {
        let mut by_cwd: HashMap<String, ClaudeProbe> = HashMap::new();
        let mut all: Vec<ClaudeProbe> = Vec::new();
        let mut by_session: HashMap<String, ClaudeProbe> = HashMap::new();
        for probe in load_dir(dir) {
            if probe.cwd.is_empty() || !probe_is_live(&probe, &observe_pid(probe.pid)) {
                continue;
            }
            all.push(probe.clone());
            if !probe.session_id.is_empty() {
                by_session.insert(probe.session_id.clone(), probe.clone());
            }
            match by_cwd.get(&probe.cwd) {
                Some(existing) if existing.status_updated_at >= probe.status_updated_at => {}
                _ => {
                    by_cwd.insert(probe.cwd.clone(), probe);
                }
            }
        }
        Self {
            by_cwd,
            all,
            by_session,
        }
    }

    /// Load from the default `~/.claude/sessions`. An unresolvable home yields
    /// an empty index, so every session falls through to the lower tiers.
    #[must_use]
    pub fn load() -> Self {
        dirs::home_dir()
            .map(|h| Self::load_from(&h.join(".claude").join("sessions")))
            .unwrap_or_default()
    }

    /// Whether any live probe was found. Drives the tier-liveness reporting.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_cwd.is_empty()
    }

    /// Number of live probes indexed.
    #[must_use]
    pub fn len(&self) -> usize {
        self.all.len()
    }

    /// Every live probe, including several sharing one cwd.
    pub fn all_live(&self) -> impl Iterator<Item = &ClaudeProbe> {
        self.all.iter()
    }

    /// The raw status a live probe reports for `cwd`, without resolving it.
    ///
    /// Exists so the caller can read the transcript for an open question ONLY
    /// for a session tier A is about to call `waiting`, rather than paying a
    /// JSONL read for every session on the host.
    #[must_use]
    pub fn peek_status(&self, session: &Session) -> Option<&str> {
        self.probe_for(session).map(|p| p.status.as_str())
    }

    /// The probe that speaks for this session: its own id when the session came
    /// from a probe, else the freshest probe in its directory.
    fn probe_for(&self, session: &Session) -> Option<&ClaudeProbe> {
        self.by_session
            .get(&session.id)
            .or_else(|| (!session.cwd.is_empty()).then(|| self.by_cwd.get(&session.cwd)).flatten())
    }

    /// Resolve one session against tier A, or `None` to abstain to tier B.
    ///
    /// Correlation is by `cwd`, the fleet's existing cross-source dedupe key —
    /// the same key `CurrentStateIndex` uses, so the two tiers agree about what
    /// a session IS even when they disagree about its state.
    #[must_use]
    pub fn resolve(
        &self,
        session: &Session,
        ask: Option<AskUserQuestionData>,
        idle_threshold_min: i64,
        now_ms: i64,
    ) -> Option<Resolution> {
        let probe = self.probe_for(session)?;
        resolve_probe(probe, session.clone(), ask, idle_threshold_min, now_ms)
    }
}

/// The exact transcript a probe names, when it exists on disk.
fn transcript_for(p: &ClaudeProbe) -> Option<String> {
    if p.session_id.is_empty() || p.cwd.is_empty() {
        return None;
    }
    let slug = crate::fleet::read::cwd_to_project_slug(&p.cwd);
    let path = dirs::home_dir()?
        .join(".claude")
        .join("projects")
        .join(slug)
        .join(format!("{}.jsonl", p.session_id));
    path.exists().then(|| path.to_string_lossy().into_owned())
}

/// Discover Claude sessions that ainb did not launch.
///
/// Every other discovery source enumerates sessions ainb (or a peer, or tmux)
/// already knows about. A Claude session started by hand, or a background
/// (`kind: "bg"`) job, appears in NONE of them — it has no ainb record, no
/// broker row, and may have no tmux pane. Its probe file is the only trace it
/// leaves, so this is the only way such a session can ever reach `fleet needs`.
///
/// This matters concretely: on the host this was written against, one session
/// had been sitting on `status: "waiting"` for eighteen days without once
/// appearing in the fleet, because nothing discovered it.
///
/// Returns only LIVE probes (pid + start-instant gated). The session id is the
/// probe's own `sessionId` so it stays stable across runs, and `summary`
/// carries Claude's session name when it has one, giving the row something a
/// human can recognise.
#[must_use]
pub fn discover_from_probes(index: &ProbeIndex) -> Vec<Session> {
    index
        .all_live()
        .map(|p| Session {
            id: if p.session_id.is_empty() {
                format!("claude-probe-{}", p.pid)
            } else {
                p.session_id.clone()
            },
            cwd: p.cwd.clone(),
            pid: Some(p.pid),
            git_root: None,
            tmux_session: None,
            workspace_name: None,
            worktree_path: None,
            peer_id: None,
            bg_job_id: None,
            // Claude names transcripts `<projects>/<cwd-slug>/<sessionId>.jsonl`
            // and the probe carries that id verbatim, so the exact transcript
            // is known. Leaving this None made the reader fall back to "newest
            // file in the cwd slug", which attributes one session's open
            // question to another whenever two run in the same directory.
            transcript_path: transcript_for(p),
            sources: vec![SessionSource::Probe],
            summary: p.name.clone().filter(|n| !n.is_empty()),
            last_seen_ms: Some(p.status_updated_at).filter(|t| *t > 0),
        })
        .collect()
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

    fn session_at(cwd: &str) -> Session {
        let mut s = session(None);
        s.cwd = cwd.to_string();
        s
    }

    /// A session carrying Claude's own id, the exact correlation key.
    fn session_ided(id: &str, cwd: &str) -> Session {
        let mut s = session_at(cwd);
        s.id = id.to_string();
        s
    }

    fn probe(status: &str, updated_at: i64) -> ClaudeProbe {
        ClaudeProbe {
            pid: 100,
            session_id: "cs1".into(),
            cwd: "/w/s1".into(),
            status: status.into(),
            name: None,
            waiting_for: None,
            status_updated_at: updated_at,
            started_at: 1_786_098_934_524,
            proc_start: "Fri Aug  7 10:35:33 2026".into(),
        }
    }

    fn live() -> PidObservation {
        PidObservation {
            alive: true,
            // "Fri Aug  7 10:35:33 2026" as UTC, +900ms of rounding.
            started_epoch_ms: Some(1_786_098_933_900),
        }
    }

    #[test]
    fn wait_row_carries_how_long_it_has_been_blocked() {
        // The real case: a session blocked for 18 days that never once showed
        // up in `fleet needs`. It must surface, WITH its age.
        let now = 1_786_106_681_993 + 18 * 24 * 60 * 60_000;
        let mut p = probe("waiting", 1_786_106_681_993);
        p.waiting_for = Some("input needed".into());
        let Some(Resolution::Hook(row)) = resolve_probe(&p, session(None), None, 5, now) else {
            panic!("an aged waiting probe must still produce a row");
        };
        let NeedsContext::Wait(w) = &row.context else {
            panic!("wrong context: {:?}", row.context);
        };
        assert_eq!(w.text, "input needed (blocked 18d)");
    }

    #[test]
    fn unusable_or_future_stamps_drop_the_age_but_keep_the_row() {
        let mut p = probe("waiting", 0);
        p.waiting_for = Some("input needed".into());
        let Some(Resolution::Hook(row)) = resolve_probe(&p, session(None), None, 5, 60_000) else {
            panic!("row must survive an unusable stamp");
        };
        assert!(matches!(&row.context, NeedsContext::Wait(w) if w.text == "input needed"));
        // A stamp from the future is clock skew, not a negative age.
        let future = probe("waiting", 900_000);
        assert!(blocked_for(&future, 60_000).is_none());
    }

    #[test]
    fn humanize_covers_minutes_hours_days() {
        assert_eq!(humanize_minutes(12), "12m");
        assert_eq!(humanize_minutes(59), "59m");
        assert_eq!(humanize_minutes(60), "1h");
        assert_eq!(humanize_minutes(60 * 24), "1d");
        assert_eq!(humanize_minutes(18 * 24 * 60), "18d");
    }

    #[test]
    fn index_keys_by_cwd_and_drops_dead_pids() {
        let dir = tempfile::tempdir().unwrap();
        let me = std::process::id();
        let started_str = chrono::DateTime::from_timestamp_millis(
            observe_pid(me).started_epoch_ms.expect("own start"),
        )
        .expect("valid instant")
        .format("%a %b %e %H:%M:%S %Y")
        .to_string();
        // Live: our own pid, so the gate genuinely passes.
        std::fs::write(
            dir.path().join("live.json"),
            format!(
                r#"{{"pid":{me},"cwd":"/w/live","status":"busy","procStart":"{started_str}","statusUpdatedAt":5}}"#
            ),
        )
        .unwrap();
        // Dead: a pid far above pid_max can never be alive.
        std::fs::write(
            dir.path().join("dead.json"),
            r#"{"pid":1073741824,"cwd":"/w/dead","status":"waiting","procStart":"Fri Aug  7 10:35:33 2026","statusUpdatedAt":5}"#,
        )
        .unwrap();
        // Malformed: the shape 6728.json really has on this box.
        std::fs::write(
            dir.path().join("bad.json"),
            r#"{"pid":1,"cwd":"/w/x"} trailing"#,
        )
        .unwrap();

        let index = ProbeIndex::load_from(dir.path());
        assert_eq!(index.len(), 1, "only the live probe is indexed");
        assert!(index.resolve(&session_at("/w/live"), None, 5, 60_000).is_some());
        assert!(
            index.resolve(&session_at("/w/dead"), None, 5, 60_000).is_none(),
            "a dead pid must not answer for its cwd"
        );
        assert!(index.resolve(&session_at("/w/x"), None, 5, 60_000).is_none());
        assert!(index.resolve(&session_at(""), None, 5, 60_000).is_none());
    }

    #[test]
    fn newest_status_wins_when_two_live_probes_share_a_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let me = std::process::id();
        let started_str = chrono::DateTime::from_timestamp_millis(
            observe_pid(me).started_epoch_ms.expect("own start"),
        )
        .expect("valid instant")
        .format("%a %b %e %H:%M:%S %Y")
        .to_string();
        for (name, id, status, stamp) in [
            ("old.json", "sess-old", "idle", 1_000),
            ("new.json", "sess-new", "busy", 9_000),
        ] {
            std::fs::write(
                dir.path().join(name),
                format!(
                    r#"{{"pid":{me},"sessionId":"{id}","cwd":"/w/same","status":"{status}","procStart":"{started_str}","statusUpdatedAt":{stamp}}}"#
                ),
            )
            .unwrap();
        }
        let index = ProbeIndex::load_from(dir.path());
        // BOTH survive for discovery — collapsing them would delete a session
        // from the fleet entirely.
        assert_eq!(index.len(), 2);
        assert_eq!(discover_from_probes(&index).len(), 2);
        // Resolving by cwd alone still takes the freshest, since cwd cannot
        // distinguish them. busy (the newer stamp) short-circuits Healthy: at
        // now=400_000 it is 6.5m old, inside the 30m freshness window, while
        // the idle sibling is 6.6m old and so past the 5m idle threshold.
        assert!(matches!(
            index.resolve(&session_at("/w/same"), None, 5, 400_000),
            Some(Resolution::Healthy)
        ));
        // Correlating by Claude's own id reaches the OTHER probe, which cwd
        // alone can never do — the case where a blocked session is masked by a
        // sibling that merely flipped status more recently. `sess-old` is idle
        // and old enough to have aged past the threshold, so it yields a row
        // where the cwd lookup yields Healthy.
        let by_id = index
            .resolve(&session_ided("sess-old", "/w/same"), None, 5, 400_000)
            .expect("the id must reach its OWN probe, not the freshest sibling");
        assert!(
            matches!(by_id, Resolution::Hook(ref row) if matches!(row.context, NeedsContext::Idle(_))),
            "id correlation must resolve the idle probe, not the busy one"
        );
    }

    #[test]
    fn a_missing_sessions_dir_yields_an_empty_index() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ProbeIndex::load_from(&dir.path().join("nope")).is_empty());
    }

    #[test]
    fn parses_the_real_probe_shape_with_unknown_fields() {
        let p = parse_probe(REAL_PROBE).expect("real probe parses");
        assert_eq!(p.pid, 29266);
        assert_eq!(p.status, "waiting");
        assert_eq!(p.waiting_for.as_deref(), Some("input needed"));
        assert_eq!(p.started_at, 1786098934524);
        assert_eq!(p.proc_start, "Fri Aug  7 10:35:33 2026");
        // procStart is UTC; startedAt is the later SESSION start.
        assert_eq!(parse_proc_start_utc(&p.proc_start), Some(1_786_098_933_000));
        assert_eq!(p.status_updated_at, 1786106681993);
    }

    #[test]
    fn corrupt_and_foreign_json_abstain_instead_of_erroring() {
        assert!(parse_probe("not json").is_none());
        assert!(parse_probe(r#"{"unrelated":true}"#).is_none()); // no pid
        assert!(parse_probe("").is_none());
        assert!(
            parse_probe(r#"{"pid":1,"cwd":"/w"#).is_none(),
            "truncated must abstain"
        );
    }

    #[test]
    fn a_trailing_brace_does_not_discard_the_whole_probe() {
        // The exact shape of ~/.claude/sessions/6728.json on the host this was
        // written against: a complete object followed by one stray `}`. Strict
        // parsing dropped the session entirely and it fell through to a pane
        // scan that reported IDLE for a session that was in fact busy.
        let p = parse_probe(r#"{"pid":6728,"cwd":"/w/x","status":"busy","startedAt":7}}"#)
            .expect("leading object must be honoured");
        assert_eq!(p.pid, 6728);
        assert_eq!(p.status, "busy");
    }

    #[test]
    fn liveness_requires_alive_pid_and_matching_start_instant() {
        let p = probe("busy", 1);
        assert!(probe_is_live(&p, &live()), "within tolerance is live");
        // Dead pid.
        assert!(!probe_is_live(
            &p,
            &PidObservation {
                alive: false,
                ..live()
            }
        ));
        // Recycled pid: alive but started at a different instant (> 2s off).
        assert!(!probe_is_live(
            &p,
            &PidObservation {
                alive: true,
                started_epoch_ms: Some(1_786_098_934_524 + 60_000)
            }
        ));
        // Unreadable start time reads as NOT live, never "probably fine".
        assert!(!probe_is_live(
            &p,
            &PidObservation {
                alive: true,
                started_epoch_ms: None
            }
        ));
        // A probe with no recorded start can never pass the gate.
        let mut blank = p;
        blank.proc_start = String::new();
        assert!(!probe_is_live(
            &blank,
            &PidObservation {
                alive: true,
                started_epoch_ms: Some(0)
            }
        ));
    }

    #[test]
    fn lstart_parses_as_local_time() {
        // Genuine LC_ALL=C ps shape, single-digit day padded by asctime.
        assert!(parse_lstart_local("Fri Aug  7 10:35:33 2026").is_some());
        assert!(parse_lstart_local("Thu Aug 20 19:53:18 2026").is_some());
        assert!(parse_lstart_local("").is_none());
        assert!(parse_lstart_local("not a date").is_none());
    }

    /// The whole gate against THIS live process: observe our own pid, write a
    /// probe naming it, and the gate must pass genuinely end to end.
    #[test]
    fn gate_passes_against_the_real_running_process() {
        let me = std::process::id();
        let obs = observe_pid(me);
        assert!(obs.alive, "the test's own pid must be alive");
        let started = obs.started_epoch_ms.expect("own lstart must parse");
        let mut p = probe("busy", 1);
        p.pid = me;
        // Round-trip our observed local start back out as the UTC asctime
        // Claude would have written, so the gate is exercised end to end.
        p.proc_start = chrono::DateTime::from_timestamp_millis(started)
            .expect("valid instant")
            .format("%a %b %e %H:%M:%S %Y")
            .to_string();
        assert!(probe_is_live(&p, &obs));
        // And a dead pid observes as such (pid 2^30 is far above pid_max).
        assert!(!observe_pid(1_073_741_824).alive);
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
        // Carries the probe's own reason, plus how long it has held it.
        assert_eq!(w.text, "input needed (blocked 0m)");
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
