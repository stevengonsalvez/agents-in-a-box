//! P1.1 RED tests for the task lifecycle FSM ([`TaskState`]).
//!
//! These exercise the legal-transition table, terminal invariants, the
//! database string round-trip, and the lowercase serde wire contract that
//! matches the Multica `agent_task_queue.status` column (`001_init.up.sql:132`).

use ainb_hangar_core::task::state::{IllegalTransition, TaskState};

/// Every variant the FSM knows about. Used to drive exhaustiveness checks so
/// adding a new variant without wiring it into the transition table fails here.
const ALL_STATES: &[TaskState] = &[
    TaskState::Queued,
    TaskState::Dispatched,
    TaskState::Running,
    TaskState::Done,
    TaskState::Failed,
    TaskState::Cancelled,
];

/// The seven legal edges the Multica spec defines (`001_init.up.sql:132`). This
/// is the *expected* set; the production [`TaskState::legal_transitions`] table
/// must equal it exactly — no more (an extra edge), no fewer (a dropped edge).
const EXPECTED_LEGAL: &[(TaskState, TaskState)] = &[
    (TaskState::Queued, TaskState::Dispatched),
    (TaskState::Dispatched, TaskState::Running),
    (TaskState::Dispatched, TaskState::Queued), // reclaim window
    (TaskState::Running, TaskState::Done),
    (TaskState::Running, TaskState::Failed),
    (TaskState::Running, TaskState::Cancelled),
    (TaskState::Queued, TaskState::Failed), // queued TTL sweep
];

#[test]
fn production_table_equals_expected_legal_set() {
    let table = TaskState::legal_transitions();

    // Length lock: an accidental 8th (or missing) edge fails on count, so a
    // divergence the per-pair check below might miss for an unmodelled variant
    // is still caught.
    assert_eq!(
        table.len(),
        EXPECTED_LEGAL.len(),
        "production TRANSITIONS has {} edges, spec defines {}",
        table.len(),
        EXPECTED_LEGAL.len(),
    );

    // Set equality in both directions.
    for edge in EXPECTED_LEGAL {
        assert!(
            table.contains(edge),
            "spec edge {edge:?} missing from production TRANSITIONS",
        );
    }
    for edge in table {
        assert!(
            EXPECTED_LEGAL.contains(edge),
            "production TRANSITIONS has unexpected edge {edge:?}",
        );
    }
}

#[test]
fn transitions_matrix_is_exhaustive() {
    // The legal set is the single production source of truth, not a local copy.
    let legal = TaskState::legal_transitions();

    // Full negative space: for EVERY `(from, to)` in the cartesian product,
    // `can_transition_to` must be `Ok` iff the pair is a legal edge. This guards
    // all illegal non-terminal edges (e.g. Queued->Running, Running->Queued)
    // that a positive-only test would silently allow, and would fail if an extra
    // edge were added to the production table.
    for &from in ALL_STATES {
        for &to in ALL_STATES {
            let is_legal = legal.contains(&(from, to));
            assert_eq!(
                from.can_transition_to(to).is_ok(),
                is_legal,
                "{from:?} -> {to:?}: can_transition_to disagrees with the legal table \
                 (expected legal={is_legal})",
            );
        }
    }

    // Exhaustiveness: every variant is reachable (appears as a `to`), and every
    // *non-terminal* variant is also departable (appears as a `from`). Terminal
    // states are sinks by design, so they only appear as a `to`.
    for &state in ALL_STATES {
        let appears_as_from = legal.iter().any(|&(f, _)| f == state);
        let appears_as_to = legal.iter().any(|&(_, t)| t == state);
        assert!(appears_as_to, "{state:?} never appears as a transition target");
        if state.is_terminal() {
            assert!(
                !appears_as_from,
                "terminal {state:?} must not appear as a transition source",
            );
        } else {
            assert!(
                appears_as_from,
                "non-terminal {state:?} never appears as a transition source",
            );
        }
    }
}

#[test]
fn terminal_states_have_no_outgoing_transitions() {
    for &terminal in &[TaskState::Done, TaskState::Failed, TaskState::Cancelled] {
        assert!(terminal.is_terminal(), "{terminal:?} should be terminal");
        for &target in ALL_STATES {
            assert!(
                matches!(
                    terminal.can_transition_to(target),
                    Err(IllegalTransition { .. })
                ),
                "terminal {terminal:?} -> {target:?} must be illegal",
            );
        }
    }
}

#[test]
fn from_db_row_round_trip_all_variants() {
    for (state, token) in [
        (TaskState::Queued, "queued"),
        (TaskState::Dispatched, "dispatched"),
        (TaskState::Running, "running"),
        (TaskState::Done, "done"),
        (TaskState::Failed, "failed"),
        (TaskState::Cancelled, "cancelled"),
    ] {
        let parsed = TaskState::from_db_str(token).expect("known token parses");
        assert_eq!(parsed, state, "{token} should parse to {state:?}");
        assert_eq!(state.as_db_str(), token, "{state:?} should render to {token}");
    }

    // An unknown string is rejected (not silently mapped to a default).
    assert!(TaskState::from_db_str("frobnicated").is_err());
    assert!(TaskState::from_db_str("Queued").is_err()); // case-sensitive
    assert!(TaskState::from_db_str("").is_err());
}

#[test]
fn serde_round_trip_lowercase() {
    // JSON wire form is the lowercase token, byte-identical to the DB column.
    for (state, token) in [
        (TaskState::Queued, "queued"),
        (TaskState::Dispatched, "dispatched"),
        (TaskState::Running, "running"),
        (TaskState::Done, "done"),
        (TaskState::Failed, "failed"),
        (TaskState::Cancelled, "cancelled"),
    ] {
        let json = serde_json::to_value(state).expect("serialize");
        assert_eq!(json, serde_json::Value::String(token.to_string()));

        // Lock the two independently-derived string paths together: the serde
        // (`rename_all = "lowercase"`) wire form must equal the hand-rolled
        // `as_db_str` token, so the DB column and JSON wire form cannot drift.
        assert_eq!(
            json.as_str().expect("serde form is a JSON string"),
            state.as_db_str(),
            "serde wire form and as_db_str disagree for {state:?}",
        );

        let back: TaskState = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, state);
    }
}

#[test]
fn is_pending_matches_partial_unique_index_predicate() {
    // `idx_one_pending_task_per_issue` (migration 022) coalesces on
    // `{queued, dispatched}`.
    assert!(TaskState::Queued.is_pending());
    assert!(TaskState::Dispatched.is_pending());
    for &s in &[
        TaskState::Running,
        TaskState::Done,
        TaskState::Failed,
        TaskState::Cancelled,
    ] {
        assert!(!s.is_pending(), "{s:?} must not be pending");
    }
}
