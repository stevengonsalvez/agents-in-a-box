//! Commit-only latency for the answer write path (spike 8, W0-wire gate).
//!
//! Spike 8 measured today's shape, three transactions per event, at a
//! commit-only p50 of **0.40 ms**, and one transaction per event at 0.14 ms
//! (`research/2026-09-11_multi-surface_SPIKE-8-sqlite-write-path.md`, section
//! "Decision input"). W0-wire adds a mutation receipt to the answer's claim.
//! The gate is that it does NOT push commit-only p50 past today's 0.40 ms,
//! which it cannot, because the receipt rides INSIDE the transaction that was
//! already committing the flip rather than opening a fourth one.
//!
//! `#[ignore]` by default. A latency assertion on a shared CI runner is a
//! flake generator, and a flaky gate gets muted, which is worse than a gate run
//! deliberately:
//!
//! ```text
//! cargo test -p ainb-hangar-store --test answer_commit_bench -- --ignored --nocapture
//! ```
//!
//! Methodology copied from spike 8 so the numbers are comparable: the real
//! crate, all migrations through the crate's own `apply_migrations`, WAL with
//! `synchronous=NORMAL` (unchanged by W0-wire), a warmup, and the COMMIT
//! statement timed on its own rather than the whole transaction.

use std::time::{Duration, Instant};

use ainb_hangar_store::Store;
use ainb_hangar_store::repo::attention::{AttentionKind, AttentionRepo, NewAttention};
use ainb_hangar_store::repo::mutation_ledger::{
    LedgerKey, MutationLedgerRepo, STATUS_ACCEPTED, TIER_RECEIPT,
};

/// Spike 8's measured commit-only p50 for the shape this replaces.
const GATE_P50: Duration = Duration::from_micros(400);
/// Enough samples that the median is a median, small enough to run in seconds.
const SAMPLES: usize = 500;
const WARMUP: usize = 50;

fn row(id: &str) -> NewAttention {
    NewAttention {
        id: id.into(),
        session_id: "bench-session".into(),
        cwd: "/work/bench".into(),
        workspace_id: None,
        kind: AttentionKind::Approval,
        payload: r#"{"kind":"APPROVAL","context":{"question":"run it?"}}"#.into(),
        degraded: false,
        created_at: 1_700_000_000_000,
        raise_transcript: None,
        channels: ainb_hangar_core::channel::ChannelSet::NONE,
    }
}

/// One answer's write boundary: the conditional flip and the receipt, in the
/// one transaction the daemon commits. Returns the COMMIT duration alone.
async fn one_answer(store: &Store, index: usize) -> Duration {
    let id = format!("att-bench-{index:06}");
    AttentionRepo::insert(store.pool(), &row(&id)).await.unwrap();
    let key = LedgerKey::local(format!("op-bench-{index:06}"));
    let fingerprint = MutationLedgerRepo::fingerprint(
        "attention/answer",
        &serde_json::json!({ "attention_id": id, "answer": "approve" }),
    );
    let now = 1_700_000_000_000 + i64::try_from(index).unwrap_or(0);

    let mut tx = store.pool().begin().await.unwrap();
    MutationLedgerRepo::claim_in_tx(
        &mut tx,
        &key,
        "attention/answer",
        &fingerprint,
        TIER_RECEIPT,
        now,
    )
    .await
    .unwrap();
    let flipped =
        AttentionRepo::mark_answered_if_open_in_tx(&mut tx, &id, "bench", "approve", now, None)
            .await
            .unwrap();
    assert_eq!(flipped, 1);
    MutationLedgerRepo::set_receipt_in_tx(&mut tx, &key, "claimed", None, now)
        .await
        .unwrap();

    let started = Instant::now();
    tx.commit().await.unwrap();
    let elapsed = started.elapsed();

    // Not timed: the terminal receipt, which is a second small transaction the
    // daemon only reaches after the PTY write returns.
    MutationLedgerRepo::record_reply(
        store.pool(),
        &key,
        STATUS_ACCEPTED,
        None,
        Some(r#"{"ok":{"outcome":"delivered"}}"#),
        now,
    )
    .await
    .unwrap();
    elapsed
}

#[tokio::test]
#[ignore = "latency bench; run with --ignored"]
async fn answer_commit_p50_holds_the_spike_8_gate() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();

    for i in 0..WARMUP {
        one_answer(&store, i).await;
    }
    let mut samples: Vec<Duration> = Vec::with_capacity(SAMPLES);
    for i in 0..SAMPLES {
        samples.push(one_answer(&store, WARMUP + i).await);
    }
    samples.sort_unstable();

    // Integer quantile indexing: 500 samples never come near a f64 mantissa
    // edge, and staying in usize keeps the bench free of cast lints that a
    // reader would have to check are harmless.
    let at = |numerator: usize, denominator: usize| {
        samples[(samples.len() * numerator / denominator).min(samples.len() - 1)]
    };
    let (p50, p95, p99, max) = (
        at(50, 100),
        at(95, 100),
        at(99, 100),
        samples[samples.len() - 1],
    );
    eprintln!(
        "answer commit-only over {SAMPLES} samples: p50 {p50:?}  p95 {p95:?}  p99 {p99:?}  max {max:?}\n\
         spike-8 baseline (three transactions per event): p50 400us"
    );
    assert!(
        p50 <= GATE_P50,
        "commit-only p50 {p50:?} exceeds the spike-8 gate of {GATE_P50:?}"
    );
}
