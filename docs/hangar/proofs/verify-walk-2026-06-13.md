# Hangar Control-Plane Verification Walk — 2026-06-13

Autonomous product verification per `docs/hangar/verify-hangar-goal.md`, run at the
close of epic `agents-in-a-box-e38` (hangar-parity).

- **Branch:** `feat/hangar-parity` · **HEAD:** `24ff1e84` · **Workspace version:** `1.6.1`
- **Env:** macOS arm64, tmux 3.6a, sqlite3 3.51.0, 22 migrations / 24 app tables
- **CI cross-reference:** Linux full 34-tripwire leg + macOS launch-smoke leg both GREEN on commit `83fb4e09`.

## Result

```
RAN=27  PASS=27  FAIL=0  SKIP=3  CI-VERIFIED=20
```

**Scope:** the deterministic, disk-light legs (real `ainb hangar` CLI + real daemon
binary + sqlite3 + a Python client doing the plugin's exact
`auth/hello`→`workspace/subscribe`→`hangar/issues_list` handshake) were RUN locally;
the heavy tmux TUI legs are CI-VERIFIED against their green tripwires on `83fb4e09`
rather than re-run on the local box.

> **DISCLOSURE — provider execution is MOCKED.** Task-execution legs (F05, F07, F16,
> F20, F21, F27, F36, R05, R06) drove the daemon claim loop against tiny `fake-claude.sh`
> scripts (happy-path, env-probe, skill-probe, slow, tempfail/exit-75, agent-error/exit-1,
> timeout-sleep), never the live `claude` binary. The FSM/DB transitions are real; the
> agent reasoning is mocked.

## Per-leg results (F01–F44 + R01–R08)

| leg | name | result | evidence |
|----|------|--------|----------|
| F01 | Issue create/list/show | PASS | create printed id; list+show render marker; DB row persisted, no placeholder |
| F02 | Issue + assignee persisted | PASS | `--assign` → issue.assignee_id == agent id; task enqueued referencing agent |
| F03 | Issue list screen | CI-VERIFIED | `tripwire_p4_issue_list_renders` |
| F04 | Kanban board + card move | CI-VERIFIED | `tripwire_kanban_columns_render` |
| F05 | Task FSM claim→terminal | PASS | queued→running→done, session_id pinned, no orphans |
| F06 | Retry chain parent/child cap | PASS | tempfail→child `parent_task_id` chained, attempt=2, capped at 2 |
| F07 | TTL sweepers | PASS | stale `dispatched` → `failed/timeout`, finished_at stamped; `sweeper_swept count=1` |
| F08 | Task detail live transcript | CI-VERIFIED | `tripwire_p4_task_detail_streams` |
| F09 | Task CLI list/cancel/retry | PASS | list shows queued; cancel→`cancelled` persisted; not re-listed |
| F10 | Task-started banner | CI-VERIFIED | task-detail/cross-screen tripwires (TUI) |
| F11 | Agent picker assign | CI-VERIFIED | `tripwire_p4_agent_picker_opens` |
| F12 | agents_list snapshot | CI-VERIFIED | inside F11 leg |
| F13 | Skill CRUD scoping/cascade | PASS | 92 skills scoped (0 foreign), cascade attach=2, skill_file content present |
| F14 | Skills sync idempotent | PASS | sync×2 → row count stable 92→92 |
| F15 | Skill manager screen | CI-VERIFIED | `tripwire_p4_skill_manager_lists` |
| F16 | Dispatch-time skill materialisation | PASS | materialised SKILL.md byte-identical to source (15673B) |
| F17/F18 | Templates list/show/use | PASS | list=10; show has instructions; `use`→agent w/ 2 cascaded skills |
| F19 | Autopilot CRUD + invalid-cron reject | PASS | good cron→row; bad cron→error, count unchanged |
| F20 | Scheduler fires on schedule | PASS | near-future tick→`autopilot_run` row; next_tick advanced |
| F21 | Scheduler skips when running | PASS | in-flight + skip policy → no new run; `tick_skipped in_flight=1 max=1` |
| F22 | Autopilots screen | CI-VERIFIED | `tripwire_autopilots_screen_renders` |
| F23 | Autopilot CLI run-now | PASS | `autopilot run`→`autopilot_run` row, status=running |
| F24 | Keychain roundtrip | SKIP | mac-keychain side effects |
| F25 | secret_store_get cap gating | SKIP | needs tampered manifest (ungranted -32001 path) |
| F26 | PAT/daemon tokens hash-only | PASS | sha256_token == sha256(plaintext); plaintext 0× in DB; create/list/revoke OK |
| F27 | Env allowlist blocks LD_PRELOAD | PASS | child env has NEITHER LD_PRELOAD nor secret; HOME/CLAUDE_HOME pass |
| F28 | danger-warning first-run | CI-VERIFIED | `tripwire_warning_shown_on_first_provider_use` |
| F29 | Workspace switching | CI-VERIFIED + R03 data-proven | `tripwire_workspace_switch_e2e` |
| F30 | Settings screen + key entry | CI-VERIFIED | `tripwire_p4_settings_renders` |
| F31/F33 | JSONL sink + spans | PASS | 11/11 valid JSON lines; spans from run_loop/runtime_register/scheduler |
| F32 | OTLP export | SKIP | needs `--features otlp` build |
| F34 | Daemon health pane + sparkline | CI-VERIFIED | `tripwire_daemon_health_sparkline` |
| F35 | Logs tail CLI + screen | PASS (CLI) / CI-VERIFIED (screen) | `logs tail` renders LOGS_TRIPWIRE_MARKER_42 ×2 + ERROR |
| F36 | PR-URL capture | PASS | fake-claude emits URL; result.pr_url = github.com/acme/widget/pull/4242 |
| F37 | PR badge + `o` open | CI-VERIFIED | `tripwire_pr_badge` |
| F38 | Daemon boot + migrations | PASS | fresh boot, 22/22 migrations applied (0 failed), expected tables present |
| F39 | Socket RPC + snapshots | PASS | plugin handshake auth/hello OK, issues_list returned both seeded markers |
| F40 | subscribe + daemon-drop detection | CI-VERIFIED | `tripwire_plugin_crash_reconnect` |
| F41 | Cross-screen nav | CI-VERIFIED | `tripwire_p4_cross_screen_navigation` (Skills=3, Autopilots=4) |
| F42 | Beads sync reconcile | PASS | `reconcile --dry-run` → `scanned=0 ... errors=0` exit 0; 0 DB writes |
| F43 | Runner env/exit/stream/timeout | PASS | env (F27), exit/stream (F05), timeout→failed (R06c) |
| F44 | Meta-guard | PASS | `tripwire_full_e2e::hangar_tripwire_suite_has_not_shrunk` ran green |
| R01 | Daemon kill-9 mid-task, restart | CI-VERIFIED | `tripwire_daemon_crash_recovery` |
| R02 | Plugin crash / host-exit no orphan | CI-VERIFIED + locally re-asserted | PPID=1 orphan grep EMPTY after every local leg |
| R03 | Multi-workspace DATA isolation | PASS | 2 non-empty ws; 0 cross-bleed both directions |
| R04 | Migration upgrade-from-populated | PASS | populated DB, 3 idempotent boots, rows intact, 22/22, 0 errors |
| R05 | Concurrent dispatch respects cap | PASS | cap=2 held at boundary; max in-flight=2, no dup ids, no deadlock |
| R06 | End-to-end retry + timeout | PASS | tempfail→retry capped; agent_error→NO child; timeout→failed NO child |
| R07 | Create-flow keystroke round trip | CI-VERIFIED | `tripwire_create_flow_round_trip` |
| R08 | Short soak | CI-VERIFIED | `tripwire_soak_stream_backpressure` (e38.32) |

## Findings (all PASS — no product defects)

1. **Claim loop is serial** (`run_loop.rs:226-249`): one task claimed + awaited to completion per poll; the `max_concurrent_tasks` cap is enforced by the `claim.rs` SQL guard (correlated COUNT of `dispatched`+`running` scoped by `agent_id` — the e38.27 over-dispatch fix). Verified at the cap boundary via sibling-runtime in-flight seeding.
2. **Skill storage vs materialisation** (F16): `skill.content` stores body-only; the dispatch-time materialiser re-writes the full `SKILL.md` byte-identical to source (verified by byte-equality).
3. **R04 method**: hand-applying migration `.sql` bypasses sqlx's `_sqlx_migrations` ledger; verified instead via the production sqlx migrator (populated DB + idempotent boots).
4. **Unauth-socket fix live** (F39): the RPC gate requires a valid `auth/hello` token on the first frame before any `hangar/*` dispatch; slug→ULID workspace resolution works for `default` and custom slugs.

## Non-product follow-up

- `toolkit/.../sentry-cli/SKILL.md` has a glued frontmatter fence (`user-invocable: true---`) that aborts `skills sync`. A data fix to that one file, outside this epic.
