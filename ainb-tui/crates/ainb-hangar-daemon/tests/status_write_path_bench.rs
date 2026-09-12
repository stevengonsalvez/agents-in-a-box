//! Spike-8 gate for the D14 write path: one transaction per event, at 100
//! sessions and 10 events/s, stays at or below 15 ms p99 WITH the attention
//! projection inside the apply transaction.
//!
//! Spike 8 measured the proposal at 11.21 ms p99 over a 20-minute run
//! (`research/2026-09-11_multi-surface_SPIKE-8-sqlite-write-path.md`, run A)
//! against a prototype. This measures the shipped code, which is the thing that
//! can regress: `FleetRepo::apply_event_with_attention` now writes the Fleet
//! event, the session state and the inbox card under one write lock, so a
//! mistake there shows up as commit latency rather than as a correctness bug.
//!
//! # The rate is part of the gate, not a detail to optimise away
//!
//! Running the events back to back looks like a strictly harsher test that
//! would finish in seconds. It is not, and the difference is large. Measured on
//! this write path:
//!
//! | arrivals | p50 | p99 | max |
//! |---|---|---|---|
//! | unthrottled, 2,000 events | 1.53 ms | 109.72 ms | 1,157.86 ms |
//! | 50/s, 2,000 events | 1.57 ms | 137.68 ms | 1,135.67 ms |
//! | **10/s, 900 events** | **1.49 ms** | **12.68 ms** | 90.69 ms |
//!
//! p50 is flat across all three, so the apply itself costs the same everywhere.
//! The tail is SQLite's automatic WAL checkpoint, which fires every 1,000 pages
//! and therefore scales with the WRITE RATE: at 10/s it lands on roughly 2 of
//! 900 commits, at 50/s on far more. A faster run does not bound a slower one,
//! it measures a different system.
//!
//! So the run is paced at the rate the spec names, and takes the ~90 seconds
//! that honestly costs. `AINB_BENCH_RATE_PER_SEC` and `AINB_BENCH_EVENTS`
//! override both, so spike 8's own run A (10/s for 20 minutes) is reproducible
//! without editing this file.

use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::attention::{AttentionKind, NewAttention};
use ainb_hangar_store::repo::fleet::{
    AttentionProjection, FleetRepo, FleetSessionPatch, NewFleetEvent, ObservationAuthority,
};

/// The ceiling this gate holds. Spike 8's own brief used 50 ms; D14 tightens it
/// to 15 ms because the projection now rides the same transaction and the
/// headroom is what pays for that.
const P99_CEILING_MS: f64 = 15.0;

/// Sessions in flight, per the spec's "100 sessions".
const SESSIONS: usize = 100;

/// Events in the run. 900 at 10/s is 90 seconds and 9 samples above p99 —
/// enough for the percentile to mean something without a 20-minute test.
const EVENTS: usize = 900;

/// Arrival rate, per the spec's "10/s". See the module header for why this is
/// not raised to shorten the run.
const RATE_PER_SEC: u64 = 10;

/// Build one hook-shaped event for `session`, with the attention projection a
/// real ask carries so the transaction does the work it does in production.
fn event_for(session: usize, sequence: usize, now_ms: i64) -> (NewFleetEvent, AttentionProjection) {
    let session_id = format!("bench-{session}");
    let session_key = format!("claude:{session_id}");
    // Alternate ask and release, which is the open/close interleaving that
    // makes the projection actually write on every event rather than settling
    // into a no-op after the first.
    let asking = sequence % 2 == 0;
    let event = NewFleetEvent {
        event_id: format!("bench:{session}:{sequence}"),
        session_key: session_key.clone(),
        observed_at: now_ms,
        authority: ObservationAuthority::Authoritative,
        event_type: if asking { "AskUserQuestion" } else { "Stop" }.to_string(),
        payload: format!(r#"{{"session_id":"{session_id}","seq":{sequence}}}"#),
        patch: FleetSessionPatch {
            provider: Some("claude".to_string()),
            provider_session_id: Some(session_id.clone()),
            cwd: Some(format!("/w/bench-{session}")),
            lifecycle_state: Some(if asking { "IDLE" } else { "TURN_COMPLETE" }.to_string()),
            attention_state: Some(if asking { "ASK" } else { "NONE" }.to_string()),
            current_request_fingerprint: Some(asking.then(|| format!("fp-{sequence}"))),
            ..FleetSessionPatch::default()
        },
    };
    let projection = AttentionProjection {
        session_id: session_id.clone(),
        close_open_asks: !asking,
        closed_by: "resolved:session".to_string(),
        closed_answer: "answered in session".to_string(),
        closed_at: now_ms,
        raise: asking.then(|| NewAttention {
            id: format!("att:{session_id}:{sequence}"),
            session_id,
            cwd: format!("/w/bench-{session}"),
            workspace_id: None,
            kind: AttentionKind::AskUserQuestion,
            payload: r#"{"kind":"ASK","context":{"question":"bench?"}}"#.to_string(),
            degraded: false,
            created_at: now_ms,
            raise_transcript: None,
            channels: ainb_hangar_core::channel::ChannelSet::NONE,
        }),
        request_key: asking.then(|| format!("rk-{session}-{sequence}")),
    };
    (event, projection)
}

/// The `percentile`-th value of `samples`, nearest-rank.
fn percentile(samples: &mut [f64], percentile: f64) -> f64 {
    samples.sort_by(|a, b| a.partial_cmp(b).expect("no NaN latencies"));
    let rank = ((percentile / 100.0) * samples.len() as f64).ceil() as usize;
    samples[rank.saturating_sub(1).min(samples.len() - 1)]
}

#[tokio::test]
async fn one_transaction_per_event_holds_the_p99_ceiling_with_the_projection_inside() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let _sink = EventBroker::new().sink();
    let rate_per_sec: u64 = std::env::var("AINB_BENCH_RATE_PER_SEC")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|rate| *rate > 0)
        .unwrap_or(RATE_PER_SEC);
    let events: usize = std::env::var("AINB_BENCH_EVENTS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|count| *count > 0)
        .unwrap_or(EVENTS);
    let interval = std::time::Duration::from_micros(1_000_000 / rate_per_sec);

    let mut latencies_ms = Vec::with_capacity(events);
    let base_ms = 1_700_000_000_000_i64;
    for sequence in 0..events {
        // Paced arrivals. Without this the run measures WAL checkpoint
        // scheduling under saturation rather than transaction cost.
        tokio::time::sleep(interval).await;
        let (event, projection) =
            event_for(sequence % SESSIONS, sequence, base_ms + sequence as i64);
        let started = std::time::Instant::now();
        FleetRepo::apply_event_with_attention(store.pool(), &event, Some(&projection))
            .await
            .expect("the apply commits");
        latencies_ms.push(started.elapsed().as_secs_f64() * 1000.0);
    }

    let p50 = percentile(&mut latencies_ms.clone(), 50.0);
    let p99 = percentile(&mut latencies_ms.clone(), 99.0);
    let max = latencies_ms.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    println!(
        "status write path: {events} events over {SESSIONS} sessions at {rate_per_sec}/s — \
         p50 {p50:.2} ms, p99 {p99:.2} ms, max {max:.2} ms (ceiling {P99_CEILING_MS:.0} ms)"
    );
    assert!(
        p99 <= P99_CEILING_MS,
        "p99 {p99:.2} ms exceeds the {P99_CEILING_MS:.0} ms ceiling \
         (p50 {p50:.2} ms, max {max:.2} ms over {events} events)"
    );

    // The bench must also have done the work it claims to time: a run that
    // silently wrote nothing would post an excellent p99.
    let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM fleet_session")
        .fetch_one(store.pool())
        .await
        .expect("count sessions");
    assert_eq!(
        usize::try_from(sessions).unwrap(),
        SESSIONS.min(events),
        "the bench must have driven the sessions it claims to"
    );
    let cards: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM attention")
        .fetch_one(store.pool())
        .await
        .expect("count attention");
    assert!(
        cards > 0,
        "the projection must have written inside the timed transaction"
    );
}
