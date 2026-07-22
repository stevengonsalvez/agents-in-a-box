# Docs review — Hangar task-failure taxonomy (issue #453)

**Verdict: PASS.** The docs fix (`f15762fe`) fully resolves issue #453. Every
claim was traced to shipped source; no fabricated behavior found. Two small
wording clarifications applied on top.

## Scope reviewed

- Branch: `ainb/01KY55VFCXAZ12D1ES5T6FQCRT`, single docs commit `f15762fe`
  (`docs/hangar/README.md` +3, `docs/hangar/tui-keybindings.md` +40).
- Source of truth (present in-branch): `ainb-hangar-store/src/service/fail.rs`,
  `.../service/retry.rs`, `ainb-hangar-daemon/src/run_loop.rs`,
  `ainb-core/src/cli/hangar/mod.rs`.

## Acceptance criteria (#453)

| Criterion | Status | Evidence |
|-----------|--------|----------|
| Every `FailureReason` variant has a one-line user-facing explanation | ✅ | All 13 variants in `fail.rs` (`timeout`, `agent_error`, `runtime_offline`, `runtime_recovery`, `user_cancel`, `iteration_limit`, `api_invalid_request`, `semantic_inactivity`, `spawn_error`, `provider_contract_drift`, `provision_error`, `spawn_timeout`, `unknown`) appear in the doc's Task-failures table |
| Retry (`R`) doc links to / states the NoRetry reasons | ✅ | `R` row links `#task-failures`; table's Retry column marks each reason |
| No fabricated behavior — traceable to source / PRs #431, #436 | ✅ | See per-claim checks below |

## Per-claim verification vs source

- **Retry dispositions** — every table entry matches `RetryService::retry_disposition` (`retry.rs:564`):
  `runtime_offline` / `runtime_recovery` → ResumeRetry; `iteration_limit` /
  `api_invalid_request` / `semantic_inactivity` → FreshRetry; all others → NoRetry.
  Cross-checked against the `retry_disposition_taxonomy` unit test — exact match.
- **`spawn_timeout` = 60s + env override** — `SPAWN_SETUP_TIMEOUT = Duration::from_secs(60)`
  and `HANGAR_SPAWN_SETUP_TIMEOUT_MS` override confirmed (`run_loop.rs:92,99`).
- **NoRetry log string** — the doc quotes `not retried (non-retryable or attempts
  exhausted)` verbatim; matches `cli/hangar/mod.rs:3263`.
- **`max_attempts` caps every chain** — matches `attempt >= max_attempts` guard
  (`retry.rs:501`).
- **Where errors surface** — `provision_error` writes the real message into
  `result` via `fail_setup` (`fail.rs:267`); `spawn_timeout` finalizes with an
  empty `RunnerResult::default()` so its cause is only in the daemon log
  (`run_loop.rs:996`), exactly as the doc's "Logs screen (`L`)" note states.
  The `L` = Logs keybinding exists (`tui-keybindings.md:26`).
- **Link integrity** — `#task-failures` anchor resolves to the `### Task failures`
  heading; README pointer to the same anchor is valid.

## Changes made in this review

Applied to `docs/hangar/tui-keybindings.md`:

1. `R` keybinding row: "the NoRetry reasons refuse" → "the reasons marked **No
   retry** there do nothing" — aligns wording with the table's own "No retry"
   label (the internal `NoRetry` enum name is not user-facing) and states the
   user-visible effect.
2. `provider_contract_drift` row: "carried no recognised terminal — a CLI shape
   the parser does not know" → "carried no recognised completion or error event
   — a CLI output shape the parser does not know" — "terminal (event)" was
   internal jargon; the rewrite is plain for a docs reader without losing accuracy.

No other wording changes needed; the rest of the section is accurate and clear.
