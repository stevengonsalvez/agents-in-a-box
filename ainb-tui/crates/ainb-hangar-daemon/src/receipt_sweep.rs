//! Boot resolution for mutation receipts a dead daemon left mid-flight
//! (spec D18, critique amendment 17).
//!
//! A receipt in `writing` is the one state the daemon cannot reason its way out
//! of after a crash: `writing` is committed immediately before the first byte
//! reaches the PTY, so the bytes may have landed or may not. Both of the tidy
//! answers are wrong:
//!
//! ```text
//!   reopen the row   ──▶ a retry types the answer a SECOND time
//!   close it quietly ──▶ a blocked agent waits forever and nobody is told
//! ```
//!
//! So the row becomes `unknown{effects_ambiguous}` and surfaces as a
//! `delivery_unconfirmed` attention row naming the answer text. Only an
//! operator closes it.
//!
//! A receipt still in `claimed` is a different fact and gets a different
//! answer: `writing` was never committed, so no byte can have reached the PTY.
//! The attention row was flipped by a claim whose delivery never happened, and
//! reopening it is exactly the compensation
//! [`crate::answer`] already performs for a failed send.

use ainb_hangar_core::clock::{HangarClock, SystemClock};
use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
use ainb_hangar_proto::mutation::{REASON_EFFECTS_AMBIGUOUS, ReceiptState};
use ainb_hangar_store::repo::attention::AttentionRepo;
use ainb_hangar_store::repo::mutation_ledger::{
    LOCAL_HOST_ID, LedgerRow, MutationLedgerRepo, TIER_RECEIPT,
};
use sqlx::SqlitePool;

/// What one boot sweep did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Receipts that were mid-write: unknowable, surfaced to an operator.
    pub unconfirmed: u64,
    /// Claims that never reached the PTY: the attention row was reopened.
    pub reopened: u64,
    /// Non-receipt claims whose handler never answered.
    pub ambiguous: u64,
}

/// Resolve every receipt and claim a prior daemon left unresolved.
///
/// Runs once at boot, before the socket accepts anything, so no client can
/// observe a half-resolved ledger. Never fatal: a sweep that cannot run leaves
/// rows unresolved, which is recoverable, whereas refusing to boot is not.
///
/// # Errors
///
/// Returns the store fault that stopped the sweep.
pub async fn run(pool: &SqlitePool) -> Result<SweepReport, sqlx::Error> {
    let now_ms = SystemClock.now_ms();
    let rows = MutationLedgerRepo::unresolved_at_boot(pool, LOCAL_HOST_ID).await?;
    let mut report = SweepReport::default();

    for row in rows {
        let receipt = row.receipt_state.as_deref().and_then(ReceiptState::from_token);
        match receipt {
            Some(ReceiptState::Writing) => {
                surface_unconfirmed(pool, &row, now_ms).await?;
                MutationLedgerRepo::resolve_unknown(
                    pool,
                    &row.key,
                    REASON_EFFECTS_AMBIGUOUS,
                    Some("the daemon stopped while the answer was being typed"),
                    now_ms,
                )
                .await?;
                report.unconfirmed += 1;
            }
            Some(ReceiptState::Claimed) => {
                report.reopened += u64::from(reopen_claimed(pool, &row, now_ms).await?);
                MutationLedgerRepo::resolve_unknown(
                    pool,
                    &row.key,
                    REASON_EFFECTS_AMBIGUOUS,
                    Some("the daemon stopped before the answer reached the session"),
                    now_ms,
                )
                .await?;
            }
            _ => {
                // A claim of any tier whose handler never answered. It may have
                // committed, so it is never re-executed; there is no answer
                // text to name, so there is nothing to surface either.
                MutationLedgerRepo::resolve_unknown(
                    pool,
                    &row.key,
                    REASON_EFFECTS_AMBIGUOUS,
                    Some("the daemon stopped before this mutation answered"),
                    now_ms,
                )
                .await?;
                report.ambiguous += 1;
            }
        }
    }
    Ok(report)
}

/// The attention row a `writing` receipt was answering, if it can still be found.
///
/// The ledger stores the op id and the method, not the target, so the link back
/// runs through the answered attention row: the claim flipped exactly one row
/// to `answered` in the same transaction that set the receipt to `claimed`, and
/// that row's `answered_at` brackets the receipt's own timestamps.
async fn answered_row(
    pool: &SqlitePool,
    row: &LedgerRow,
) -> Result<Option<ainb_hangar_store::repo::attention::AttentionRow>, sqlx::Error> {
    if row.tier != TIER_RECEIPT {
        return Ok(None);
    }
    let Some(id) = attention_id_of(row) else {
        return Ok(None);
    };
    AttentionRepo::get(pool, &id).await
}

/// The attention id this receipt belongs to, read from the detail the claim
/// wrote.
///
/// The ledger keys on the op id, which says nothing about the target, so the
/// claim writes the attention id into `receipt_detail` in the same transaction
/// as the flip. A row without one is still resolved, just not surfaced: an
/// operator alert naming nothing is worse than none.
fn attention_id_of(row: &LedgerRow) -> Option<String> {
    let raw = row.receipt_detail.as_deref()?;
    let parsed: serde_json::Value = serde_json::from_str(raw).ok()?;
    parsed
        .get("attention_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

/// Raise the `delivery_unconfirmed` row an operator has to close.
async fn surface_unconfirmed(
    pool: &SqlitePool,
    row: &LedgerRow,
    now_ms: i64,
) -> Result<(), sqlx::Error> {
    let answered = answered_row(pool, row).await?;
    let payload = serde_json::json!({
        "kind": "DELIVERY_UNCONFIRMED",
        "op_id": row.key.op_id,
        "method": row.method,
        "context": {
            "answer": answered.as_ref().and_then(|a| a.answer.clone()),
            "answered_by": answered.as_ref().and_then(|a| a.answered_by.clone()),
            "attention_id": answered.as_ref().map(|a| a.id.clone()),
            "detail": row.receipt_detail,
        },
    })
    .to_string();

    let id = SystemIdGen.new_ulid();
    let session_id = answered
        .as_ref()
        .map_or_else(|| row.key.op_id.clone(), |a| a.session_id.clone());
    let cwd = answered.as_ref().map_or("", |a| a.cwd.as_str());
    let workspace_id = answered.as_ref().and_then(|a| a.workspace_id.as_deref());
    AttentionRepo::insert_delivery_unconfirmed(
        pool,
        &id,
        &session_id,
        cwd,
        workspace_id,
        &payload,
        now_ms,
    )
    .await?;
    tracing::warn!(
        op_id = %row.key.op_id,
        method = %row.method,
        attention_id = %id,
        "a mutation was mid-write when the daemon stopped; raised delivery_unconfirmed"
    );
    Ok(())
}

/// Put back the attention row a claim flipped but never delivered.
async fn reopen_claimed(
    pool: &SqlitePool,
    row: &LedgerRow,
    now_ms: i64,
) -> Result<u32, sqlx::Error> {
    let Some(answered) = answered_row(pool, row).await? else {
        return Ok(0);
    };
    let Some(answered_by) = answered.answered_by.as_deref() else {
        return Ok(0);
    };
    let reverted = AttentionRepo::reopen(pool, &answered.id, answered_by, now_ms).await?;
    if reverted > 0 {
        tracing::warn!(
            op_id = %row.key.op_id,
            attention_id = %answered.id,
            "an answer was claimed but never reached the session; reopened the request"
        );
    }
    Ok(u32::try_from(reverted).unwrap_or(u32::MAX))
}
