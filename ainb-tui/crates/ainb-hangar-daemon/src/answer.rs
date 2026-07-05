//! The daemon's answer router — deliver an answer to one open attention row from
//! any surface, exactly once, into the right session (architecture §4.3, spec P2).
//!
//! Every surface (TUI control centre, web, bridge, ATC) answers through the same
//! `attention/answer` RPC, which lands here. The router enforces the two
//! guarantees the converged inbox promises:
//!
//! 1. **First-answer-wins (exactly-once):** the `open → answered` flip is a
//!    conditional UPDATE ([`AttentionRepo::mark_answered_if_open`]). Two surfaces
//!    answering the same row serialise at the database; exactly one flips it and
//!    delivers, the other is told "already answered by X" and delivers nothing.
//!
//! 2. **C1 misroute refusal:** an answer must reach the EXACT agent that asked.
//!    The hook's session id often differs from a discovered [`Session::id`], so an
//!    exact-id miss falls back to a cwd correlation — but two agents can share a
//!    cwd, so an ambiguous cwd would risk sending the answer to the WRONG agent.
//!    The router REFUSES on ambiguity (more than one discovered session in the
//!    cwd, or a merged session that aggregated 2+ sources) rather than guess —
//!    the guard lifted from the TUI fleet panel's `control::resolve_and_send`.
//!    Attention rows are also DURABLE and outlive the raising agent, so the
//!    router additionally binds the cwd fallback to the transcript captured at
//!    raise time: if the original session has exited and a DIFFERENT agent now
//!    occupies the cwd (its transcript is newer), the router refuses rather than
//!    answer an agent that never asked.
//!
//! Ordering matters: the C1 target resolution runs BEFORE the flip, so an
//! ambiguous or dead target leaves the row OPEN and answerable later (a session
//! may exit, or the ambiguity may resolve). Only once a unique target is known
//! does the router claim the row (win-or-lose) and deliver via the one verified
//! send path ([`send`], the multi-line-submit-verified tmux path).

use ainb_fleet_core::discover::{discover_from_ainb, discover_from_peers, merge_sessions};
use ainb_fleet_core::read::jsonl_tail::latest_transcript_for_cwd;
use ainb_fleet_core::send::send;
use ainb_fleet_core::types::{SendOutcome, Session};
use ainb_hangar_proto::events::HangarEvent;
use ainb_hangar_proto::snapshots::{AnswerParams, AnswerResult};
use ainb_hangar_store::repo::attention::{AttentionRepo, AttentionRow};
use sqlx::SqlitePool;

use crate::events::EventSink;

/// The resolved delivery target for an answer, or a refusal.
enum Target {
    /// A single, unambiguous live session to deliver into.
    Send(Session),
    /// The C1 guard refused: the target could not be resolved unambiguously.
    Ambiguous(String),
    /// No live session matched (the target may have exited).
    NoTarget(String),
}

/// Answer one open attention row: guard, claim (first-answer-wins), deliver.
///
/// Returns a tagged [`AnswerResult`] the caller serialises back to the surface;
/// a store fault propagates as a [`sqlx::Error`] the RPC layer maps to an
/// internal error. A successful delivery emits an `AttentionAnswered` event on
/// the fleet-wide attention stream so every surface moves the card to
/// `answered(by=…)`.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if reading the row or the conditional flip fails.
pub async fn answer(
    pool: &SqlitePool,
    events: &EventSink,
    params: &AnswerParams,
    now_ms: i64,
) -> Result<AnswerResult, sqlx::Error> {
    // Load the row to recover the raising session's id + cwd (and to short-circuit
    // an already-answered row before any discovery I/O).
    let Some(row) = AttentionRepo::get(pool, &params.attention_id).await? else {
        return Ok(AnswerResult::NoTarget {
            reason: "no such attention row (already resolved or never existed)".to_string(),
        });
    };
    if row.state != "open" {
        return Ok(AnswerResult::AlreadyAnswered {
            by: row.answered_by.unwrap_or_else(|| "unknown".to_string()),
        });
    }

    // C1: resolve the delivery target BEFORE claiming, so an ambiguous / dead
    // target leaves the row open and answerable later.
    match resolve_target(
        &row.session_id,
        &row.cwd,
        row.raise_transcript.as_deref(),
        params.is_answer,
    )
    .await
    {
        Target::Ambiguous(reason) => Ok(AnswerResult::Ambiguous { reason }),
        Target::NoTarget(reason) => Ok(AnswerResult::NoTarget { reason }),
        Target::Send(session) => {
            // Claim the answer. A second surface that also resolved a target loses
            // this flip (0 rows) and delivers nothing.
            let flipped = AttentionRepo::mark_answered_if_open(
                pool,
                &params.attention_id,
                &params.answered_by,
                &params.answer,
                now_ms,
            )
            .await?;
            if flipped == 0 {
                let by = AttentionRepo::get(pool, &params.attention_id)
                    .await?
                    .and_then(|r| r.answered_by)
                    .unwrap_or_else(|| "unknown".to_string());
                return Ok(AnswerResult::AlreadyAnswered { by });
            }

            // We won: deliver via the one verified send path. Only a CONFIRMED
            // delivery (`Tmux` / `Broker`) keeps the row answered + emits the
            // `AttentionAnswered` nudge. On a delivery fault (`Failed`, or an
            // `Err`) the claim is COMPENSATED — the row is reverted to `open` so
            // the still-blocked agent's request stays in the inbox and remains
            // answerable, rather than leaving the feed forever on a transient
            // tmux/broker miss.
            match send(&session, &params.answer).await {
                Ok(SendOutcome::Tmux { tmux_session }) => {
                    emit_answered(events, params);
                    Ok(AnswerResult::Delivered {
                        via: format!("tmux ({tmux_session})"),
                    })
                }
                Ok(SendOutcome::Broker { peer_id }) => {
                    emit_answered(events, params);
                    Ok(AnswerResult::Delivered {
                        via: format!("broker ({peer_id})"),
                    })
                }
                Ok(SendOutcome::Failed { reason }) => {
                    reopen_on_failed_delivery(pool, events, &row, params, now_ms).await?;
                    Ok(AnswerResult::DeliveryFailed { reason })
                }
                Err(e) => {
                    reopen_on_failed_delivery(pool, events, &row, params, now_ms).await?;
                    Ok(AnswerResult::DeliveryFailed {
                        reason: e.to_string(),
                    })
                }
            }
        }
    }
}

/// Emit the `AttentionAnswered` nudge on the fleet-wide attention stream.
fn emit_answered(events: &EventSink, params: &AnswerParams) {
    events.emit_attention(HangarEvent::AttentionAnswered {
        attention_id: params.attention_id.clone(),
        by: params.answered_by.clone(),
    });
}

/// Undo a claim whose last-mile delivery failed: revert the row to `open` and
/// re-raise it on the attention stream so every surface shows it as answerable
/// again. Scoped to this caller's own claim ([`AttentionRepo::reopen`]), so a
/// concurrent winner is never clobbered.
async fn reopen_on_failed_delivery(
    pool: &SqlitePool,
    events: &EventSink,
    row: &AttentionRow,
    params: &AnswerParams,
    now_ms: i64,
) -> Result<(), sqlx::Error> {
    let reverted =
        AttentionRepo::reopen(pool, &params.attention_id, &params.answered_by, now_ms).await?;
    if reverted > 0 {
        // Re-nudge so live surfaces re-show the card without waiting for the
        // next snapshot pull. Best-effort (dropped when there are no subscribers).
        events.emit_attention(HangarEvent::AttentionRaised {
            attention_id: row.id.clone(),
            session_id: row.session_id.clone(),
            workspace_id: row.workspace_id.clone(),
            kind: row.kind.as_str().to_string(),
            degraded: row.degraded,
            created_at: row.created_at,
            // Re-nudge carries the row's ORIGINAL raise-time channels — a reopen
            // must not re-resolve (that would let a mid-flight rule edit change
            // where an in-flight attention routes), so the fan-out decision stays
            // fixed at first raise.
            channels: row.channels,
        });
    }
    Ok(())
}

/// Resolve a `(session_id, cwd)` to a single live [`Session`] to deliver into, or
/// a refusal — the C1 ambiguity guard lifted from `ainb-core`'s
/// `fleet::control::resolve_and_send`.
///
/// Discovery runs both sources in real parallel (`discover_from_ainb` is async;
/// `discover_from_peers` is a blocking SQLite read run on a blocking thread).
/// Resolution priority + guard:
///   1. EXACT session-id match → always safe (unambiguously the named agent).
///   2. No exact match → correlate by cwd, but ONLY when that cwd is unambiguous
///      AND the raising session still OWNS it. Ambiguous = more than one
///      discovered session shares the cwd, OR the merged session aggregated 2+
///      sources. Stale = the raising session's captured transcript is no longer
///      the newest in the cwd (a different agent took over the durable row's cwd
///      after the original exited). On ambiguity or staleness → refuse.
async fn resolve_target(
    session_id: &str,
    cwd: &str,
    raise_transcript: Option<&str>,
    is_answer: bool,
) -> Target {
    let ainb_fut = discover_from_ainb();
    let peers_fut = tokio::task::spawn_blocking(discover_from_peers);
    let (ainb, peers_join) = tokio::join!(ainb_fut, peers_fut);
    let ainb: Vec<Session> = ainb.unwrap_or_default();
    let peers: Vec<Session> = peers_join.ok().and_then(Result::ok).unwrap_or_default();

    // Count raw (pre-merge) sessions sharing the target cwd across BOTH sources,
    // so two distinct sessions in the same dir register as ambiguous even though
    // `merge_sessions` would coalesce them onto one cwd-keyed row.
    let raw_cwd_count = if cwd.is_empty() {
        0
    } else {
        ainb.iter().chain(peers.iter()).filter(|s| s.cwd == cwd).count()
    };

    let merged = merge_sessions(vec![ainb, peers]);

    // 1. Exact session-id match is always unambiguous.
    if let Some(session) = merged.iter().find(|s| s.id == session_id) {
        return Target::Send(session.clone());
    }

    // 2. No exact match → cwd correlation, guarded by ambiguity.
    let Some(by_cwd) = merged.iter().find(|s| !cwd.is_empty() && s.cwd == cwd) else {
        return Target::NoTarget("no live session matched (target may have exited)".to_string());
    };

    let ambiguous = raw_cwd_count > 1 || by_cwd.sources.len() > 1;
    if ambiguous {
        let label = if is_answer {
            "cannot safely answer"
        } else {
            "refusing to send"
        };
        return Target::Ambiguous(format!(
            "ambiguous target — {label} ({} sessions in this cwd)",
            raw_cwd_count.max(by_cwd.sources.len())
        ));
    }

    // C1 temporal guard: attention rows are durable and outlive the raising
    // agent, so a purely-cwd fallback can misroute the answer to a DIFFERENT
    // agent that later occupied the same cwd. Bind delivery to the transcript
    // captured at raise time — if it is no longer the newest transcript in the
    // cwd, the occupant changed; refuse rather than answer an agent that never
    // asked. (A row with no captured transcript keeps the prior behaviour.)
    if let Some(raise_tx) = raise_transcript.filter(|t| !t.is_empty()) {
        if !transcript_still_owns_cwd(cwd, raise_tx) {
            let label = if is_answer {
                "cannot safely answer"
            } else {
                "refusing to send"
            };
            return Target::Ambiguous(format!(
                "stale target — {label} (the raising session no longer owns this cwd)"
            ));
        }
    }

    Target::Send(by_cwd.clone())
}

/// Does the session that raised the request still OWN `cwd`? True when the newest
/// transcript in the cwd's project dir is still the one captured at raise time.
///
/// Compared by file NAME — the session-unique transcript id — so the hook's
/// absolute path and the discovered path never disagree over formatting. A
/// missing current transcript (dir gone) is treated as NOT owning (refuse).
fn transcript_still_owns_cwd(cwd: &str, raise_transcript: &str) -> bool {
    let raise_name = std::path::Path::new(raise_transcript).file_name();
    raise_name.is_some()
        && latest_transcript_for_cwd(cwd).is_some_and(|current| current.file_name() == raise_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_fleet_core::types::SessionSource;
    use ainb_hangar_store::Store;
    use ainb_hangar_store::repo::attention::{AttentionKind, NewAttention};

    fn session(id: &str, cwd: &str, src: SessionSource) -> Session {
        Session {
            id: id.to_string(),
            cwd: cwd.to_string(),
            pid: None,
            git_root: None,
            tmux_session: Some("tmux".to_string()),
            workspace_name: None,
            worktree_path: None,
            peer_id: None,
            bg_job_id: None,
            transcript_path: None,
            sources: vec![src],
            summary: None,
            last_seen_ms: None,
        }
    }

    /// The C1 resolution decision, isolated from `send()` I/O so it is
    /// deterministically unit-testable — a verbatim mirror of `resolve_target`'s
    /// steps 1+2 + the ambiguity guard (lifted from `fleet::control`).
    #[derive(Debug, PartialEq)]
    enum Decision {
        Exact(String),
        Cwd(String),
        Refuse,
        NoMatch,
    }

    fn resolve_decision(raw: &[Session], session_id: &str, cwd: &str) -> Decision {
        let merged = merge_sessions(vec![raw.to_vec()]);
        if let Some(s) = merged.iter().find(|s| s.id == session_id) {
            return Decision::Exact(s.id.clone());
        }
        let raw_cwd_count = if cwd.is_empty() {
            0
        } else {
            raw.iter().filter(|s| s.cwd == cwd).count()
        };
        let Some(by_cwd) = merged.iter().find(|s| !cwd.is_empty() && s.cwd == cwd) else {
            return Decision::NoMatch;
        };
        if raw_cwd_count > 1 || by_cwd.sources.len() > 1 {
            return Decision::Refuse;
        }
        Decision::Cwd(by_cwd.id.clone())
    }

    #[test]
    fn exact_session_id_match_delivers() {
        let raw = vec![
            session("hook-sid", "/work/x", SessionSource::Ainb),
            session("other", "/work/x", SessionSource::Peers),
        ];
        assert_eq!(
            resolve_decision(&raw, "hook-sid", "/work/x"),
            Decision::Exact("hook-sid".to_string())
        );
    }

    #[test]
    fn unambiguous_cwd_match_delivers() {
        let raw = vec![session("discovered-id", "/work/x", SessionSource::Ainb)];
        assert_eq!(
            resolve_decision(&raw, "hook-session-differs", "/work/x"),
            Decision::Cwd("discovered-id".to_string())
        );
    }

    #[test]
    fn ambiguous_cwd_two_raw_sessions_refuses() {
        let raw = vec![
            session("a", "/work/x", SessionSource::Ainb),
            session("b", "/work/x", SessionSource::Ainb),
        ];
        assert_eq!(
            resolve_decision(&raw, "hook-sid", "/work/x"),
            Decision::Refuse
        );
    }

    #[test]
    fn ambiguous_cwd_multi_source_merge_refuses() {
        let raw = vec![
            session("a", "/work/x", SessionSource::Ainb),
            session("a", "/work/x", SessionSource::Peers),
        ];
        assert_eq!(
            resolve_decision(&raw, "hook-sid", "/work/x"),
            Decision::Refuse
        );
    }

    /// Plant a transcript `<name>` under a UNIQUE cwd's `~/.claude/projects`
    /// slug dir. Returns the cwd, the planted file path, and a cleanup guard.
    struct TxFixture {
        cwd: String,
        dir: std::path::PathBuf,
    }
    impl Drop for TxFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
    fn plant_transcript(name: &str) -> (TxFixture, std::path::PathBuf) {
        use std::io::Write;
        let cwd = format!(
            "/ainb-test-answer-c1/{}/{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        );
        let mut dir = dirs::home_dir().expect("home dir");
        dir.push(".claude");
        dir.push("projects");
        dir.push(ainb_fleet_core::read::jsonl_tail::cwd_to_project_slug(&cwd));
        std::fs::create_dir_all(&dir).expect("create project dir");
        let file = dir.join(name);
        writeln!(std::fs::File::create(&file).unwrap(), "{{}}").unwrap();
        (TxFixture { cwd, dir }, file)
    }

    #[test]
    fn transcript_owns_cwd_when_it_is_the_newest() {
        let (fx, file) = plant_transcript("session-a.jsonl");
        // The captured transcript IS the newest in the cwd → the raiser owns it.
        assert!(transcript_still_owns_cwd(&fx.cwd, file.to_str().unwrap()));
        // A stale/never-there transcript name → not owning.
        assert!(!transcript_still_owns_cwd(
            &fx.cwd,
            "/anywhere/session-gone.jsonl"
        ));
    }

    #[test]
    fn transcript_does_not_own_cwd_after_a_newer_session_takes_over() {
        let (fx, original) = plant_transcript("session-original.jsonl");
        // A DIFFERENT agent starts in the same cwd, writing a newer transcript.
        std::thread::sleep(std::time::Duration::from_millis(10));
        let newcomer = fx.dir.join("session-newcomer.jsonl");
        std::fs::write(&newcomer, "{}\n").unwrap();

        // The original raiser's transcript is no longer the newest → refuse.
        assert!(
            !transcript_still_owns_cwd(&fx.cwd, original.to_str().unwrap()),
            "a newer session in the cwd means the original no longer owns it"
        );
        // The newcomer (had it been the raiser) would own it.
        assert!(transcript_still_owns_cwd(
            &fx.cwd,
            newcomer.to_str().unwrap()
        ));
    }

    #[test]
    fn no_session_in_cwd_is_no_match_not_refuse() {
        let raw = vec![session("a", "/other", SessionSource::Ainb)];
        assert_eq!(
            resolve_decision(&raw, "hook-sid", "/work/x"),
            Decision::NoMatch
        );
    }

    #[test]
    fn empty_cwd_without_exact_match_is_no_match() {
        let raw = vec![session("a", "", SessionSource::Ainb)];
        assert_eq!(resolve_decision(&raw, "hook-sid", ""), Decision::NoMatch);
    }

    // --- answer() store-integration paths that need no live discovery ---------

    async fn seed_ws_and_open_row(pool: &SqlitePool, id: &str, session_id: &str, cwd: &str) {
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
            .bind("ws-a")
            .bind("ws-a")
            .bind("ws-a")
            .bind(1_000_i64)
            .execute(pool)
            .await
            .unwrap();
        AttentionRepo::insert(
            pool,
            &NewAttention {
                id: id.to_string(),
                session_id: session_id.to_string(),
                cwd: cwd.to_string(),
                workspace_id: Some("ws-a".to_string()),
                kind: AttentionKind::AskUserQuestion,
                payload: "{}".to_string(),
                degraded: false,
                created_at: 1_000,
                raise_transcript: None,
                channels: ainb_hangar_core::channel::ChannelSet::NONE,
            },
        )
        .await
        .unwrap();
    }

    fn broker_sink() -> (crate::events::EventBroker, EventSink) {
        let broker = crate::events::EventBroker::new();
        let sink = broker.sink();
        (broker, sink)
    }

    #[tokio::test]
    async fn answering_a_missing_row_is_no_target() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let (_b, sink) = broker_sink();
        let params = AnswerParams {
            attention_id: "nope".into(),
            answer: "x".into(),
            answered_by: "tui".into(),
            is_answer: true,
        };
        let res = answer(store.pool(), &sink, &params, 5000).await.unwrap();
        assert!(matches!(res, AnswerResult::NoTarget { .. }));
    }

    #[tokio::test]
    async fn answering_an_already_answered_row_reports_the_winner() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_ws_and_open_row(store.pool(), "a1", "sid", "/work/x").await;
        // A prior answer already resolved it.
        AttentionRepo::mark_answered_if_open(store.pool(), "a1", "web", "first", 4000)
            .await
            .unwrap();

        let (_b, sink) = broker_sink();
        let params = AnswerParams {
            attention_id: "a1".into(),
            answer: "second".into(),
            answered_by: "tui".into(),
            is_answer: true,
        };
        let res = answer(store.pool(), &sink, &params, 5000).await.unwrap();
        match res {
            AnswerResult::AlreadyAnswered { by } => assert_eq!(by, "web"),
            other => panic!("expected AlreadyAnswered, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_failed_delivery_reopens_the_claimed_row() {
        // Simulate the winning-flip-then-failed-send sequence: claim the row,
        // then run the compensation the send-failure arms. The row must return to
        // the open feed so the still-blocked agent's request stays answerable.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_ws_and_open_row(store.pool(), "a1", "sid", "/work/x").await;
        let (_b, sink) = broker_sink();
        let params = AnswerParams {
            attention_id: "a1".into(),
            answer: "option 2".into(),
            answered_by: "tui".into(),
            is_answer: true,
        };

        // Win the flip (as answer() does before the last-mile send).
        assert_eq!(
            AttentionRepo::mark_answered_if_open(store.pool(), "a1", "tui", "option 2", 5000)
                .await
                .unwrap(),
            1
        );
        let row = AttentionRepo::get(store.pool(), "a1").await.unwrap().unwrap();

        // The send failed → compensate.
        reopen_on_failed_delivery(store.pool(), &sink, &row, &params, 5000)
            .await
            .unwrap();

        let after = AttentionRepo::get(store.pool(), "a1").await.unwrap().unwrap();
        assert_eq!(
            after.state, "open",
            "a failed delivery must not strand the row"
        );
        assert!(after.answered_by.is_none());
        // A later, live re-answer is therefore possible (row is open again).
        assert_eq!(
            AttentionRepo::list_fleet(store.pool()).await.unwrap().len(),
            1,
            "the reopened request is back on the fleet feed"
        );
    }

    #[tokio::test]
    async fn open_row_with_no_live_target_stays_open() {
        // A unique, guaranteed-nonexistent session id + cwd: no discovered session
        // (real or otherwise) can match, so resolve_target is deterministically
        // NoTarget regardless of what the host's live roster is. The row must NOT
        // be claimed — an undeliverable answer leaves it answerable later.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let nonce = format!(
            "{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        );
        let cwd = format!("/no/such/cwd/{nonce}");
        seed_ws_and_open_row(
            store.pool(),
            "a1",
            &format!("no-such-session-{nonce}"),
            &cwd,
        )
        .await;

        let (_b, sink) = broker_sink();
        let params = AnswerParams {
            attention_id: "a1".into(),
            answer: "x".into(),
            answered_by: "tui".into(),
            is_answer: true,
        };
        let res = answer(store.pool(), &sink, &params, 5000).await.unwrap();
        assert!(
            matches!(res, AnswerResult::NoTarget { .. }),
            "no live session matches the unique id/cwd → NoTarget"
        );
        // The row is still open — we never claimed an undeliverable answer.
        let row = AttentionRepo::get(store.pool(), "a1").await.unwrap().unwrap();
        assert_eq!(
            row.state, "open",
            "an unresolved answer leaves the row open"
        );
    }
}
