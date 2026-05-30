// ABOUTME: Merge raw signals into a coarse SessionState.
//
// Priority:
//   1. api-error  -> block = ApiError (triggers daemon auto-continue)
//   2. ask-user-question OR needs-input-marker -> block = NeedsInput
//   3. waiting-summary -> block = WaitingSummary
//   4. else -> block = None
//
// Liveness: turn-active > turn-end > idle > unknown.

use crate::fleet::types::{Block, Liveness, Session, SessionState, Signal};

#[must_use]
pub fn derive_state(session: Session, signals: Vec<Signal>) -> SessionState {
    let liveness = pick_liveness(&signals);
    let block = pick_block(&signals);
    let last_activity_ms = signals
        .iter()
        .map(signal_at)
        .max()
        .or(session.last_seen_ms);

    SessionState {
        session,
        liveness,
        block,
        last_assistant_snippet: None,
        last_activity_ms,
        signals,
    }
}

fn signal_at(s: &Signal) -> i64 {
    match s {
        Signal::TurnEnd { at }
        | Signal::TurnActive { at }
        | Signal::AskUserQuestion { at, .. }
        | Signal::WaitingSummary { at, .. }
        | Signal::NeedsInputMarker { at, .. }
        | Signal::ApiError { at, .. }
        | Signal::Idle { at, .. } => *at,
    }
}

fn pick_liveness(signals: &[Signal]) -> Liveness {
    if signals.iter().any(|s| matches!(s, Signal::TurnActive { .. })) {
        Liveness::MidTurn
    } else if signals
        .iter()
        .any(|s| matches!(s, Signal::TurnEnd { .. } | Signal::Idle { .. }))
    {
        Liveness::Idle
    } else {
        Liveness::Unknown
    }
}

fn pick_block(signals: &[Signal]) -> Block {
    if signals.iter().any(|s| matches!(s, Signal::ApiError { .. })) {
        return Block::ApiError;
    }
    if signals.iter().any(|s| {
        matches!(
            s,
            Signal::AskUserQuestion { .. } | Signal::NeedsInputMarker { .. }
        )
    }) {
        return Block::NeedsInput;
    }
    if signals
        .iter()
        .any(|s| matches!(s, Signal::WaitingSummary { .. }))
    {
        return Block::WaitingSummary;
    }
    Block::None
}
