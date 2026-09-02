# Hangar execution map (2026-09-02, at 7d2b10d3)

Appendix to `docs/hangar/renovation/PLAN.md`. Produced by a read-only code archaeology pass; every claim carries a file:line. Paths are relative to the worktree the pass ran in; the tree is the same on `main` after PR #815.

Mode: focused-query

# Hangar task execution + surface inventory: `agents-in-a-box` @ `7d2b10d3` (branch `f/prove-hangar`, 2026-09-02)

All paths absolute. Base: `/home/claude/.agents-in-a-box/worktrees/by-name/agents-in-a-box--f-prove-hangar--22bb34f4/ainb-tui/`

Repo identity (confirmed):
- `git rev-parse HEAD` → `7d2b10d3c54dae28930247627f97089a74898640`
- `git remote get-url origin` → `https://github.com/stevengonsalvez/agents-in-a-box.git`
- `git log -1` → `7d2b10d3 2026-09-02 16:47:08 +0000 steven gonsalvez`

---

## 1. How a hangar task executes: and where ACP fits

**Plain answer: tasks execute as local CLI subprocesses (`claude` / `codex` / `copilot` / `agy`), spawned by the daemon's claim loop. ACP is used ONLY for fleet CHAT sessions: it never touches the task path.** Confirmed: `rg -in 'acp'` returns ZERO matches in both `run_loop.rs` and `runner.rs`.

```
┌──────────┐  issue_run /   ┌──────────────┐  PULL_SQL   ┌───────────────┐
│ TUI/CLI  │  board_card_run│  run_card()  │────────────▶│agent_task_queue│
└──────────┘───────────────▶└──────────────┘  (or direct)└───────┬───────┘
                                                                 │ CLAIM_SQL
                              ┌──────────────────────────────────▼────────┐
                              │ run_loop: provision wt → env → sandbox    │
                              └───┬───────────────────────────┬───────────┘
                       headless   │                           │ interactive
                    ┌─────────────▼──────────┐   ┌────────────▼──────────────┐
                    │ claude -p --output-    │   │ tmux new-session -d       │
                    │ format stream-json     │   │ `tmux_hangar-<task_id>`   │
                    │ → {logs}/claude.jsonl  │   │ (NO sandbox, NO capture)  │
                    └────────────────────────┘   └───────────────────────────┘
```

### a. issue → task row

| Step | Location |
|---|---|
| `hangar/issue_run` handler | `.../crates/ainb-hangar-daemon/src/rpc/mod.rs:9651` |
| `hangar/board_card_run` handler | `.../crates/ainb-hangar-daemon/src/rpc/mod.rs:9541` |
| Shared launch core `run_card` | `.../crates/ainb-hangar-daemon/src/rpc/mod.rs:10148` |
| Mode validation (`""`/`headless`/`interactive`, else invalid_params) | `rpc/mod.rs:9661-9669` |
| Tenant guard (issue must exist in this workspace) | `rpc/mod.rs:9672-9676` |
| Brief-or-link guard: **scoped to `issue_run` only**, NOT the shared core | `rpc/mod.rs:9684-9690` (test pins the asymmetry at `rpc/mod.rs:15648`) |
| Remote-only repo favourite resolved to a LOCAL clone before dispatch | `rpc/mod.rs:9698-9706` |
| Agent / assignee / source-branch / invoker overrides | `rpc/mod.rs:9707-9723` |
| `run_card(...)` invocation, board tier skipped via `board_id = None` | `rpc/mod.rs:9724-9738` |
| Result shape `BoardCardRunResult` (single vs squad) | `rpc/mod.rs:9748-9777` |
| Squad fan-out vs single enqueue | `CardRunOutcome` at `rpc/mod.rs:9783`; fan-out service `.../crates/ainb-hangar-store/src/service/squad_assign.rs:319` |

**Pull pipeline** (`.../crates/ainb-hangar-store/src/service/pull.rs`): a `board_card` in a role-gated column IS the queue. `PullService::pull_for_runtime` materialises at most ONE `agent_task_queue` row per tick via a single `INSERT…SELECT…RETURNING` (`PULL_SQL`, `pull.rs:531`; executed at `pull.rs:206`), gated on six predicates documented at `pull.rs:44-80`:

1. Role gate: the column declares `services_role` and the agent holds it. `services_role IS NULL` (Backlog, Done, pre-0074 columns) is not a pull queue at all.
2. WIP limit: column has fewer than `wip_limit` cards holding an active task; `NULL` = unlimited.
3. One owner per card: the card's issue has NO active (`queued`/`dispatched`/`running`) task.
4. Stage not already finished: the card's CURRENT column holds no `done` task at the card's CURRENT generation (the FINALIZE WINDOW guard; needs `board_column_id`, migration 0078).
5. Prior-agent exclusion: on `excludes_prior_agent = 1`, an agent with a `done` task on this card may not take it (reviewer ≠ implementer). Failure direction: the card WAITS rather than being self-reviewed.
6. Not blocked + agent has capacity: unfinished `card_dependency` blockers, and `max_concurrent_tasks`.

Why the row is created by the puller and not a dispatcher: `agent_task_queue.agent_id` is `NOT NULL`, so a queued row always already names its agent; true pull needs the agent chosen at claim time, and making the column nullable is a full table rebuild on SQLite (`pull.rs:11-20`). Atomicity rests on SQLite serialising writes so a concurrent puller's sub-select observes this statement's committed row (`pull.rs:33-43`). Mutation proofs live at `.../crates/ainb-hangar-store/tests/pull_role_gate.rs`: they delete each clause verbatim from `PULL_SQL` and assert the forbidden pull then succeeds.

**The row**: `agent_task_queue`, base schema `.../crates/ainb-hangar-store/migrations/0004_agent_task_queue.sql:22-39`, then **13 further migrations `ALTER`ing it**. Base columns: `id, workspace_id, runtime_id, agent_id, issue_id, status, result, session_id, work_dir, attempt, max_attempts, parent_task_id, failure_reason, created_at, started_at, finished_at`. `status` is CHECK-constrained to `queued|dispatched|running|done|failed|cancelled` (`0004…sql:28`). A partial unique index enforces ≤1 pending task per issue (`0004…sql:47-49`).

Columns added later by ALTER: `agent_kind, autopilot_run_id, board_column_id, branch, dispatched_at, generation, mode, origin_id, origin_type, priority, repo_ref, run_group, session_name, source_branch, squad_id, target_branch, token_budget, trigger_comment_id`.

The `Task` struct is 28 fields (`.../crates/ainb-hangar-store/src/repo/task.rs:77`):
`id, workspace_id, runtime_id, agent_id, issue_id, status, result, session_id, work_dir, attempt, max_attempts, parent_task_id, failure_reason, priority, created_at, dispatched_at, started_at, finished_at, autopilot_run_id, mode, session_name, repo_ref, agent_kind, branch, generation, source_branch, squad_id, origin`.

**Retry**: `maybe_spawn_retry` → `RetryService::maybe_retry_failed` inserts a CHILD task row (`run_loop.rs:2218-2240`). A collision with the per-(issue, agent) pending-unique index is treated as benign "already pending", not a fault (`run_loop.rs:2234-2237`).

**Auto-run**: only via dependency unblock: `board.rs:439` reads `CardDependencyRepo::get_auto_run`, then `auto_run_dependent` (`.../crates/ainb-hangar-daemon/src/board.rs:452`). Card-level flag also set through `hangar/board_card_set_auto_run`.

### b. Claim loop

Entry `run_loop::run` at `.../crates/ainb-hangar-daemon/src/run_loop.rs:457`.

Boot sequence: `log_sandbox_posture` (`run_loop.rs:469`, added because an exit-65 dispatch failure was ambiguous between "fix present, still failing" and "stale binary"), `spawn_sweepers` (`:470`), `spawn_gc_sweeper` (`:477`), `spawn_runtime_presence` (`:488`, deliberately before the `disable_claim` early-return), then `reclaim_orphans_on_restart` (`:517`) gated on the migration-0092 instance-id arrival so a re-registered runtime does not double-dispatch its own live runs.

Per-tick order: **pull first, then claim**: deliberate, so a handoff to the next stage is one poll interval rather than two (`run_loop.rs:598-621`). A pull fault degrades the daemon to push-only rather than downing the loop (`run_loop.rs:617-621`). Then `ClaimTaskService::claim_for_runtime` (`run_loop.rs:623`).

Executions spawn onto a `JoinSet` (`run_loop.rs:551`, `:633`) so a long-lived interactive run never wedges the claim loop; the per-agent `max_concurrent_tasks` cap needs no in-memory accounting because the claim SQL counts the agent's live `dispatched`+`running` rows (`run_loop.rs:536-544`). Shutdown reaps interactive tmux sessions by exact name on SIGINT only (SIGTERM is `daemon stop`/`restart`, whose panes are re-adopted by the next boot's reconciler) then `runs.shutdown()` awaits every aborted run so `kill_on_drop(true)` SIGKILLs land before return (`run_loop.rs:558-581`).

Per-task path in `execute_claimed` (`run_loop.rs:1087`):

- `workspace_slug` + `prepare_env` → task execenv tree (`run_loop.rs:1107-1115`). A setup fault **terminalises** via `finalize_setup_failure` rather than propagating, because a propagated error left the row `dispatched` and the stale-dispatch sweeper re-dispatched it into the same fault, looping invisibly until the 5min TTL relabelled it `timeout` with no cause (`run_loop.rs:1100-1106`).
- One `CLAUDE.md` materialised in the execenv carrying the workspace context prompt AND (for a squad-LEADER task) the claim-time briefing; append-only, both parts best-effort (`run_loop.rs:1117-1145`).
- `dispatch_mode(&task.mode)`: the ONE place the row's `mode` column becomes a `Mode`, carried on `ResolvedDispatch` so the exec-path branch and the argv can never disagree (`run_loop.rs:1155-1159`).
- `resolve_dispatch` (agent → runtime → provider + model/cli_args/agent_env) at `run_loop.rs:1160-1172`; a resolve fault falls back to the default provider WITH the fallback prompt and is logged, never silently substituted.
- Worktree provision `crate::workdir_provision::provision` (`run_loop.rs:1210-1225`). Two slugs: the FULL task id keys the volatile worktree (`~/.agents-in-a-box/worktrees/<taskid>` on branch `ainb/<id>`), while SCRATCH is keyed on `(issue, agent)` so reruns reuse a durable dir and squad members get distinct ones (`run_loop.rs:1195-1209`).
- Typed FSM guard `LifecycleGuard::claimed()` (`run_loop.rs:1236`), cancel registry registered BEFORE the start transition to close the race window (`run_loop.rs:1238-1242`), then `dispatched → running` via `StartTaskService::start` (`run_loop.rs:1248-1264`). A cancel that won the race tears down the worktree and returns without running (`run_loop.rs:1255-1262`).
- **Spawn-setup umbrella**: every await between the `running` commit and the provider spawn is bounded by ONE timeout (`run_loop.rs:1295-1325`): the board auto-move, the issue-lifecycle advance, the "started" progress comment, and `prepare_spawn_inputs` (`run_loop.rs:972`) which includes `resolve_cred_env` (`run_loop.rs:928`) with its own `CRED_READ_TIMEOUT`. Expiry → `FailureReason::SpawnTimeout`, classified `NoRetry` (`run_loop.rs:1329-1354`). Rationale at `run_loop.rs:1280-1294`: a wedge anywhere in that span was the forever-`running` black hole.
- Credential resolution `.../crates/ainb-hangar-daemon/src/claude_cred.rs::resolve`: 4-step ladder documented in `ainb-tui/CLAUDE.md`: `HANGAR_CLAUDE_OAUTH_TOKEN` → system `claude` login read by shelling to `/usr/bin/security` → legacy stored token → nothing (run reaches `claude` and fails loudly). Backend built ONCE for the daemon's lifetime and shared across runs (`run_loop.rs:545-549`), injectable as a trait object for tests.
- Env composition: `compose_child_env` in `.../crates/ainb-hangar-daemon/src/runner.rs`: allowlist filter over the source env plus agent_env layered on top; agent_env cannot shadow the daemon parent stamp (pinned by unit tests in the same file).
- Sandbox: `.../crates/ainb-hangar-sandbox/src/lib.rs`: Seatbelt profile via `/usr/bin/sandbox-exec` on macOS (`imp_macos`), Landlock ruleset applied in a `pre_exec` hook on Linux (`imp_linux`), `Enforcement::None` passthrough elsewhere so the confinement test SKIPs rather than vacuously passing (`lib.rs:20-35`). Default ON on Linux / OFF on macOS (`run_loop.rs:295-303`); `HANGAR_DAEMON_DISABLE_SANDBOX` overrides: `"1"` forces OFF, `"0"` forces ON, anything else falls back to the platform default (`run_loop.rs:278-284`). Wired into `RunnerConfig` at `run_loop.rs:530-533`. Provider paths are canonicalised to absolute ONCE at config time because a bare `claude` emits a `(literal "claude")` Seatbelt rule the kernel never matches, denying exec of the real PATH-resolved binary (`run_loop.rs:191-218`, `resolve_provider_path:320`).

### c. Execution backends

Provider specs in `.../crates/ainb-hangar-daemon/src/runner.rs` (2421 lines): `claude_spec:1295`, `codex_spec:1368`, `copilot_spec:1468`, `antigravity_spec:1495`. Dispatch match at `run_loop.rs:1405-1449`.

**Headless claude argv** (`runner.rs:1296-1320`):
`-p --dangerously-skip-permissions --output-format stream-json --verbose [--model M] [cli_args…] -- <prompt>`
Constants at `runner.rs:246` (`-p`), `:303-305` (`--output-format stream-json`), `:306-309` (`--verbose`, hard-required by `--print` + stream-json). `structured: mode == Mode::Headless` (`runner.rs:1327`): only the headless argv promises a structured terminal to finalize on, so the runner finalizes on claude's OWN reported outcome rather than the exit code (`runner.rs:1306-1309`).

**Headless codex** (`runner.rs:1368-1399`): `exec --skip-git-repo-check -s danger-full-access --json [-m MODEL] [cli_args…] -- <prompt>`. Interactive omits ALL of `exec`, `--skip-git-repo-check` and the sandbox flag: `--skip-git-repo-check` is an `exec`-only flag and a hard parse error at top level (`runner.rs:1347-1361`). Retired model ids are substituted at the wire because a retired id dispatches into a blocking migration modal a headless run has nobody to dismiss (`runner.rs:1381-1394`).

**Copilot** (`runner.rs:1468`, prompt flag `-p` at `:315`) carries `--allow-all-tools`, mandatory for non-interactive mode (`runner.rs:1291-1294`). **Antigravity** (`runner.rs:1495`): `agy -p --dangerously-skip-permissions --output-format stream-json [--model M] [cli_args…] -- <prompt>` headless; interactive drops `-p` and stream-json and adds `-i` (`runner.rs:1491-1495`, argv pinned by the unit test at `:2370`).

**Output capture** (headless): `{env.logs}/{spec.log_file}` opened at `runner.rs:1571-1572`, stdout streamed line-by-line by `stream_stdout` (`runner.rs:1873-1953`) which appends each line to the log file AND to a bounded oldest-line-evicting ring tail (`TAIL_LINES`), read concurrently so it never blocks the claim loop. One log file per provider so a run of both keeps transcripts separate: `claude.jsonl` (`:1323`), `codex.jsonl` (`:1403`), `copilot.jsonl` (`:1482`), `antigravity.jsonl` (`:1518`).

**Interactive** (`run_interactive`, `run_loop.rs:1559`; argv derived solely by `interactive_command`, `run_loop.rs:1538`): detached tmux session named **`tmux_hangar-<task_id>`** (`.../crates/ainb-hangar-daemon/src/interactive.rs:63`). **Deliberately unsandboxed and un-token-injected** so the attached claude reaches the Keychain and `~/.claude` natively and auto-refreshes; a static `CLAUDE_CODE_OAUTH_TOKEN` would override keychain auth and go stale mid-session (`run_loop.rs:1381-1386`). Completion is detected by the session being reaped, mapped onto the same `RunOutcome` the headless path returns (`run_loop.rs:1357-1361`). There is **no structured output capture on this path**: the pane is the sole artifact. `interactive_command` was extracted precisely because a prior shape hardcoded `Mode::Headless` inside `run_interactive` and kept 254 lib tests green while reintroducing the regression; the only coverage was a tmux tripwire that SKIPs when tmux is absent (`run_loop.rs:1530-1537`).

Session naming and teardown use exact-name targeting (`=name`) because a bare `-t` resolves exact then prefix, so reaping `ainb-run-abc` could kill a live `ainb-run-abc2` (`interactive.rs:74-78`).

**Brief composition** (`build_prompt`, `run_loop.rs:2548-2594`):
```
[stage_prompt]

<issue.title>

<issue.description>

Linked issue: <issue.external_ref>
```
Falls back to the agent's own instructions (with the stage layer stacked below), then `FALLBACK_PROMPT` (`run_loop.rs:2584-2593`). The stage prompt is the migration-0076 `board_column.stage_prompt` of the card's column, constrained to PIPELINE boards (boards carrying ≥1 role-gated column) so a personal kanban board carding the same issue can never inject its own text; within a pipeline board the gating stage wins and `board_id` breaks ties (`stage_prompt_for_issue`, `run_loop.rs:2596-2627`). Best-effort: a store fault degrades to "no stage layer", never strands a dispatch.

Acceptance criteria are NOT part of the brief: no `acceptance` read appears in `build_prompt`. (`acceptance_criterion_state` is migration 0054 and `hangar/issue_criterion_set`, a separate surface.)

**Race + cancel**: `provider_run` is raced against the cancel token with `biased` ordering so an already-signalled cancel wins deterministically (`run_loop.rs:1452-1493`). Headless cancel drops the future, firing `kill_on_drop(true)` to SIGKILL the process group; interactive kills the detached session by exact name, since a dropped `wait` does not kill it (`run_loop.rs:1456-1469`). A provider that cannot be spawned/exec'd is converted to a terminal `FailureReason::SpawnError` (`NoRetry`) rather than propagating, because propagation left the row frozen `running` until the multi-hour TTL (`run_loop.rs:1470-1492`).

**Terminal → FSM**: `finalize_success:1698` / `finalize_failure:1912` / `finalize_setup_failure:2095` / `finalize_cancelled:2177`, each preceded by a typed `lifecycle.fire(...)` edge (`run_loop.rs:1498-1520`).

**PR capture**: `ainb_hangar_core::pr_url::parse_gh_pr_create_stdout` scans the **bounded stdout tail** for a canonical `gh pr create` URL, last one wins; a no-PR run yields `None` and the key is omitted entirely so the JSON stays byte-identical to the pre-P9 shape (`run_loop.rs:1708-1720`).

**Finalize order on success** (`run_loop.rs:1731-1837`): `CompleteTaskService::complete` → `persist_usage` → `persist_run_branch` → `record_run_history` (+ OTLP task→run span) → `stats.record_completed` → `emit_task_finished` → `board::auto_move_after_terminal` → `board::record_task_outcome` → `board::advance_and_cascade_child` → `board::unblock_dependents_after_terminal` → progress comment → `teardown_workdir` (keep-if-dirty). A cancel that won the finalize race still tears down and records history, then returns Ok (`run_loop.rs:1746-1773`).

**Issue lifecycle advance**: `advance_issue_lifecycle_after_transition` (`.../crates/ainb-hangar-daemon/src/board.rs:126-201`) maps `running → in_progress`, `done → Done`, and no-ops on `failed`/`cancelled`. Advance-only by `IssueLifecycle::advance_rank` (NOT `order()`, because `blocked` paints at column 5 right of `done` and comparing column order would freeze a blocked issue forever: `board.rs:115-119`); never revives a terminal issue (`board.rs:156-161`). A cancelled TASK deliberately does not set its ISSUE to `cancelled`: that is a human decision, not a consequence of one killed run (`board.rs:140-143`). `advance_issue_lifecycle_after_terminal` (`board.rs:225`) gates on the AGGREGATE drain via `TaskRepo::issue_aggregate_terminal_state`, so a squad member finishing does not mark the whole issue done; and a finished pipeline STAGE is explicitly not a finished ISSUE (`board.rs:215-219`).

**Squad fan-out**: `SquadAssignService::assign_fanout` (`.../crates/ainb-hangar-store/src/service/squad_assign.rs:319`) resolves and validates every dispatch target before touching the queue. Note the historical direction: fan-out wrote one `agent_task_queue` row per member, so one issue became N simultaneous runs in N worktrees: which is the problem `PullService` was built to replace (`pull.rs:1-8`).

### d. Where ACP fits: **it does not run tasks**

| Fact | Evidence |
|---|---|
| ACP absent from the task path | `rg -in acp run_loop.rs runner.rs` → 0 matches (confirmed) |
| ACP entry points are fleet-only | `.../crates/ainb-hangar-daemon/src/acp_pool.rs:1-16` names `fleet/message_send` and `fleet/action` as the two inbound edges |
| ACP session creation mints a `fleet_session`, not a task | `rpc/mod.rs:5947-5989`: `provider: "acp"`, `management_state: MANAGED`, `lifecycle_state: IDLE`, `capabilities: acp_capabilities()` |
| Provider token | `ACP_PROVIDER_TOKEN = "acp"` at `acp_pool.rs:160`; the concrete adapter lives in `fleet_acp_session.provider` (`rpc/mod.rs:5964-5967`) |
| Providers that speak ACP | exactly two: `claude-agent-acp`, `codex-acp` (`.../crates/ainb-acp/src/config.rs:11-13`); unknown tokens refused at `rpc/mod.rs:5934-5941` |
| Architecture | MULTIPLEXED: ONE adapter process per PROVIDER hosting many sessions, demuxed by ACP `sessionId` (`acp_pool.rs:1-16`, decided 2026-08-04) |
| `ainb-acp` is a pure library | no `EventBroker`, no socket, no raw SQL: fenced by `.../crates/ainb-acp/tests/store_fence.rs` asserting `sqlx` never reaches its dependency set (`.../crates/ainb-acp/src/lib.rs:17-24`) |
| Capability gate | `require_fleet_capability(FLEET_CAPABILITY_ACP_SPAWN)` at `rpc/mod.rs:5918` |

Crate shape (`.../crates/ainb-acp/src/lib.rs:1-16`, 2887 LOC src / 1131 tests): `client` (spawns the adapter, speaks the wire via the upstream `agent-client-protocol` crate: nothing re-implements the protocol), `reducer` (normalises `session/update` into `TranscriptChunk`s), `store_writer` (batches into `fleet_provider_event` rows), `reprime` (resume prelude for adapters that cannot `session/load`, re-exported from `ainb-hangar-proto`), `circuit` (per-provider-process crash breaker).

Adapter child env is built from nothing (`env_clear`) then filled from `BASE_ENV_ALLOWLIST = ["PATH", "HOME"]` plus configured passthrough/extra, because the spike observed a `bypassPermissions` leak via ambient-state inheritance (`config.rs:16-23`). `permission_mode` is PINNED at `session/new` and re-asserted after every `session/load` (`config.rs:32-38`).

Five load-bearing properties, each with a test (`acp_pool.rs:19-68`): exact demux (an unclaimed `sessionId` is DROPPED, never cross-attributed); at-most-once prompt delivery; convergence is not boot-only; a turn ends in ONE transaction (receipt gates the reply); work is bounded but notifications are not (the `session/update` and `session/request_permission` demux channels are deliberately unbounded, with `transcript_bytes` on `hangar/daemon_health` as the observability ceiling).

Approval attention rows: `raise_permission` at `acp_pool.rs:2513`, inserting `AttentionKind::Approval` (`acp_pool.rs:2549`); answered through `fleet/action` → `execute_acp_action` (`rpc/mod.rs:6088`), which reaches the adapter's **pending JSON-RPC id**. Convergence retires ghost rows on adapter death or turn end (`acp_pool.rs:1449-1456`). ACP sessions are exempt from the request-fingerprint staleness gate because an adapter running parallel tool calls raises several `session/request_permission` at once and the session row has room for exactly one; the pool's `parked` map is the authority (`rpc/mod.rs:3966-3980`).

**So: tasks execute as sandboxed CLI subprocesses with stream-json/tmux capture; ACP is used for fleet chat sessions (`fleet_session.provider = "acp"`) and their tool-permission approvals.**

### e. Human-in-the-loop: screen-scraping, with one structured island

```
 claude hook ──▶ ainb fleet atc hook ──▶ events.jsonl ──▶ attention_ingest
                                                              │
                                          attention row ◀─────┘
                                                │
   TUI Control Center ──── attention/answer ────┤
                                                ▼
                                       daemon answer.rs
                                     capture_pane + route_answer
                                                │
                            ┌───────────────────┴──────────────┐
                            ▼                                  ▼
                 tmux send-keys -l  <text>          tmux send-keys <digit>
                 (+ observed Enter)                 (picker by POSITION)
```

**Outbound (agent → human):**
- Hook writer: `.../crates/ainb-core/src/cli/fleet/atc.rs:1358` (`hook`), inner `:1843`, core `:1891`/`:1917`, canonical line builder `:2440`, appender `:2545` (`<home>/events.jsonl`). PIPE_BUF atomicity constraint on the max embedded payload at `atc.rs:2340-2342`: the file is appended concurrently by many processes. Errors are swallowed to keep the Stop hook non-blocking (`atc.rs:1405`).
- Ingest: `.../crates/ainb-hangar-daemon/src/attention_ingest.rs:1-55` (2116 lines). The daemon owns its OWN byte-offset cursor and reads the shared file directly rather than cross-reading notifyd's rusqlite, so the store boundary holds (`attention_ingest.rs:8-11`). Qualifying events: `Notification` / `Stop` / `SubagentStop`, run through the same `classify` (ASK > ERR > IDLE > WAIT) the fleet panel uses.
- Idempotency id `att:<session>:<event_id>`; legacy lines fall back to byte offset. The cursor is a pure efficiency optimisation, not a correctness dependency (`attention_ingest.rs:28-42`). ASK rows additionally carry a `request_key` (migration 0084) because Claude re-fires `Notification` while a session stays blocked and **one live question was measured raising three cards** (`attention_ingest.rs:46-53`).
- AskUserQuestion options are read from the **hook payload**, not the transcript, because Claude withholds the picker's `tool_use` row until the tool RESOLVES: a transcript read while the question is open finds nothing and no interview could ever raise a card (`attention_ingest.rs:21-25`).
- ATC escalations flow through the SAME attention pipeline (`.../crates/ainb-hangar-daemon/src/atc.rs:75-131`), idempotent on `escalation:<instance>:<session>:<now>`, emitting `HangarEvent::AttentionRaised`.
- `attention` table: migration 0025 (`attention.sql:57`), `kind` CHECK-constrained to six families (`0025…sql:30-62`), plus 0026 (raise transcript) and 0084 (request key).

**Inbound (human → agent):**
- Router: `.../crates/ainb-hangar-daemon/src/answer.rs:66` (`answer`, 1461 lines total). Two guarantees stated at `answer.rs:5-30`: first-answer-wins via the conditional `mark_answered_if_open` UPDATE, and C1 misroute refusal: an exact-id miss falls back to cwd correlation, but ambiguity (2+ sessions in the cwd, or a merged session aggregating 2+ sources) is REFUSED rather than guessed, and the cwd fallback is bound to the transcript captured at raise time so a different agent now occupying the cwd cannot be answered. Target resolution runs BEFORE the flip so an ambiguous/dead target leaves the row open.
- `Route` enum `answer.rs:153-163`: `Picker(usize)` (drive the option picker by zero-based position), `Text` (type it), `Refuse(String)` (deliver nothing, reopen the row).
- `route_answer` `answer.rs:176-215` refuses in FOUR distinct pane conditions: (a) a free-text ASK while some picker is on screen, (b) a picker on screen that is not this row's picker, (c) an option index ≥ 9 ("pickers beyond 9 options are not supported"), (d) an answer matching none of the picker's labels.
- **Why position and not text** (`answer.rs:168-175`): an `AskUserQuestion` picker does not read typed text: typing the chosen label and pressing Enter "accepted whatever option was HIGHLIGHTED, so the store recorded the operator's pick while the agent acted on the default."
- `deliver` `answer.rs:218-245`: the pane is read whenever keys would go into one, picker or not; a pane the daemon cannot read while the row says a picker is up is NOT typed into blind: the row stays open for a surface that can see the screen.
- Transport: `ainb_fleet_core::send::send` (`.../crates/ainb-fleet-core/src/fleet/send/route.rs:152-180`): `TmuxFirst` / `TmuxOnly` / `PeersFirst`. Only two channels exist: tmux, and an HTTP broker to a peer `ainb` (which itself delivers via tmux). On `NotSubmitted` the broker is deliberately NOT tried, since the text is already parked in the composer and would be delivered twice (`route.rs:155-158`).
- The measured basis for the scraping is documented at `.../crates/ainb-fleet-core/src/fleet/send/tmux.rs:1-60` (against claude 2.1.224 / tmux 3.6a / macOS, with captures kept in `pane_fixtures/`): a macOS PTY delivers at most 1022 bytes per `read()`; Claude Code classifies a single read of ≥801 chars as a PASTE; whether the trailing Enter submits depends on whether the CR lands in its OWN read: on a busy target it fuses into the payload's final read and is consumed as a literal newline, so nothing submits; retried Enters at +1.0s/+1.7s/+3.9s all arrived fused as `\r\r\r`, so **there is no safe fixed delay: the wait must be OBSERVED, and observed on the payload**. Payload goes out as ONE literal `send-keys -l` write after a `--` end-of-options terminator (without it a text starting with `-`, e.g. an interview label `--no`, is parsed as a flag and silently dropped). Verification looks at composer EMPTINESS rather than the payload, because `ainb run` creates every session at 80x24 and the composer viewport is tail-anchored: a 1546-byte payload rendered as 7 rows / 382 visible chars with its leading marker OFF-SCREEN. "Clear" must be dim-aware: an idle Claude renders a dim ghost of the previous prompt, a busy one renders a dim "Press up to edit queued messages" AFTER accepting the send, and in plain `capture-pane -p` both are byte-identical in shape to real input.

**Is there any structured channel?** Repo-wide searches return **ZERO** hits for `--input-format`, `control_request`, `control_response`, `canUseTool`, `can_use_tool` across `*.rs` / `*.md` / `*.ts` / `*.py`. No Claude Code SDK dependency exists: `@anthropic-ai/claude-code` appears only as an install-catalog package name (`.../crates/ainb-core/src/setup/catalog.rs:391`, `.../crates/ainb-core/src/config/registry.rs:1994`, `.../crates/ainb-core/src/config/container.rs:155`, `.../crates/ainb-core/src/setup/installer.rs:322`).

Structured HITL exists in exactly two places, **neither on the task path**:
1. **ACP** `session/request_permission` → parked responder → attention row → `fleet/action` → `execute_acp_action` (`rpc/mod.rs:6088`) → the adapter's pending JSON-RPC id.
2. **Codex app-server**, a persistent WebSocket JSON-RPC transport with one actor owning the reader and writer (`.../crates/ainb-hangar-daemon/src/fleet_provider/codex_manager.rs:1-6`, 3701 lines) → `execute_codex_action` (`rpc/mod.rs:4663`). When the managed transport is not active it falls back to `verified_tmux_send` (`rpc/mod.rs:4066-4074`).

`answer.rs` contains **zero** `acp`/`Acp` matches (confirmed by `rg -c`). The I8 arm in `fleet/action` exists precisely because without it an ACP `SendPrompt` fell into `verified_tmux_send` and reported "exact tmux process identity is unavailable" for a session that has no tmux pane by design (`rpc/mod.rs:4056-4062`).

---

## 2. Surface inventory (counted)

**JSON-RPC methods**: `.../crates/ainb-hangar-proto/src/methods.rs` (2180 lines): **157 `pub const` method constants**, 159 distinct wire strings in the file. **154 dispatch arms** in the daemon (`rpc/mod.rs`, 16706 lines); the TUI plugin references **75**.

| Prefix | Count |
|---|---|
| `hangar/` | 111 |
| `fleet/` | 33 |
| `atc/` | 4 |
| `profile/` | 3 |
| `attention/` | 3 |
| `workspace/` | 2 |
| `codex/` | 2 |
| `auth/` | 1 |

Full method list by prefix:

- **atc/** (4): `escalate`, `list`, `register`, `unregister`
- **attention/** (3): `answer`, `list`, `subscribe`
- **auth/** (1): `hello`
- **codex/** (2): `session_discard`, `session_ensure`
- **profile/** (3): `get`, `list`, `upsert`
- **workspace/** (2): `list`, `subscribe`
- **fleet/** (33): `acp_session_create`, `action`, `activity_event`, `activity_list`, `broadcast`, `channel_create`, `channel_list`, `confirm_answer`, `confirm_event`, `confirm_list`, `copilot_configure`, `copilot_gate`, `message_event`, `message_list`, `message_send`, `message_subscribe`, `negotiate`, `quota_summary`, `receipt_get`, `receipt_list`, `reproject_claude_interview`, `resync_required`, `runtime_status`, `snapshot`, `start`, `subscribe`, `timeline`, `transcript_event`, `transcript_list`, `transcript_prune`, `transcript_subscribe`, `usage_dashboard`, `usage_summary`
- **hangar/** (111): `agent_archive`, `agent_create`, `agent_delete`, `agent_skills_list`, `agent_update`, `agents_list`, `autopilot_collaborator_add`, `autopilot_collaborator_remove`, `autopilot_collaborators`, `autopilot_fire_now`, `autopilot_runs`, `autopilot_set_access_mode`, `autopilot_set_api_trigger`, `autopilot_set_enabled`, `autopilot_subscriber_add`, `autopilot_subscriber_remove`, `autopilot_subscribers`, `autopilot_trigger_api`, `autopilot_update`, `autopilot_versions`, `autopilots_list`, `board_card_add`, `board_card_assign_squad`, `board_card_cancel`, `board_card_create`, `board_card_dep_add`, `board_card_dep_remove`, `board_card_move`, `board_card_remove`, `board_card_reorder`, `board_card_run`, `board_card_set_auto_run`, `board_card_timeline`, `board_column_add`, `board_column_delete`, `board_column_reorder`, `board_column_update`, `board_create`, `board_delete`, `board_update`, `boards_list`, `comment_add`, `comment_mention_preview`, `daemon_config_get`, `daemon_config_list`, `daemon_config_set`, `daemon_health`, `dispatch_attempts_list`, `health`, `inbox_list`, `inbox_mark_read`, `invite_accept`, `invite_create`, `invite_decline`, `invite_revoke`, `issue_cancel_active`, `issue_create`, `issue_criterion_set`, `issue_delete`, `issue_label_attach`, `issue_label_detach`, `issue_link_add`, `issue_link_remove`, `issue_links`, `issue_metadata_delete`, `issue_metadata_get`, `issue_metadata_set`, `issue_property_clear`, `issue_property_set`, `issue_reaction_add`, `issue_reaction_remove`, `issue_run`, `issue_subscribe`, `issue_subscribers`, `issue_timeline`, `issue_unsubscribe`, `issue_update`, `issues_batch_update`, `issues_list`, `issues_search`, `member_remove`, `member_set_role`, `members_list`, `notify_rule_set`, `notify_rules_list`, `pr_status_refresh`, `properties_list`, `property_archive`, `property_define`, `repo_list`, `run_history`, `search`, `skill_attach`, `skill_detach`, `skill_get`, `skill_set_enabled`, `skills_list`, `skills_sync`, `squad_archive`, `squad_assign`, `squad_create`, `squad_fanout`, `squad_instructions_set`, `squad_member_add`, `squad_member_remove`, `squad_member_role_set`, `squads_list`, `task_retry`, `task_transition`, `tasks_list`, `usage_rollup`

**TUI screens**: `.../crates/ainb-plugin-hangar/src/screen/`: **20 `Screen` variants** (`screen/mod.rs:58`) across **26 modules**.

Variants: `IssueList`(1), `TaskDetail(TaskId)`(2), `AgentPicker(IssueId)`, `ActivityTimeline(IssueId)`(y), `SkillManager`(3), `Autopilots`(4), `Kanban`(K), `Boards`(B), `DaemonHealth`(D), `Usage`(U), `Logs`(L), `Inbox`(I), `ControlCenter`(C), `Fleet`(F), `Squads`(S), `Profiles`(P), `Agents`(A), `Settings`(,), `Help`(?), `CommandPalette`(Ctrl+P).

Modules: `activity, agent_picker, agents, app_screens, autopilots, banner_state, boards, command_palette, context_menu, control_center, daemon_health, fleet, fleet_chat, inbox, issue_list, kanban, list_context_menu, logs, mod, profiles, router, settings, skill_manager, squads, task_detail, usage_dashboard`.

`ROUTER_KEYS` = **18 chars**: `1 2 3 4 B K D U L I C F S P A , ? q` (`screen/router.rs:30`). `HOST_RESERVED_KEYS` is empty (`router.rs:42`) because `ainb-core` lists `hangar-tui` in `PLUGINS_WITH_OWN_HELP`. An invariant test (`router_keys_all_have_a_reduce_key_arm`) keeps the const and `reduce_key` in lock-step, and `is_reserved_key` (`router.rs:75`) exists because a screen-local binding on a router key is DEAD and an advertised hint on it lies to the user (issue #450).

**CLI**: `ainb hangar` (`.../crates/ainb-core/src/cli/hangar/mod.rs:79`, 16175 lines): **18 top-level noun groups**, 32 `Subcommand` enums, **152 total variants**, 29 of which are group nodes (`#[command(subcommand)]`) → **123 executable leaf commands**.

| Group | Leaves |
|---|---|
| `issue` | 18 (Create, List, Search, Show, Update, BatchState, Delete, Label, Criteria, Link, Subscribe, Unsubscribe, Subscribers, React, Why, Timeline, Property, Meta) + nested: Property 2, Meta 4, Link 3, React 3, Criteria 3, Label 2 |
| `autopilot` | 14 + AutopilotActor 3 |
| `daemon` | 10 + Cred 3, Config 3 |
| `squad` | 10 |
| `agent` | 9 |
| `member` | 9 |
| `skills` | 5 |
| `workspace` | 4 |
| `pipeline` | 3 (Init, Show, StagePrompt) |
| `property` | 3 |
| `templates` | 3 |
| `auth` | 2 + Token 3, DaemonToken 1 |
| `config` | 2 + EnvAllow 3, Warnings 1 |
| `comment` | 2 |
| `task` | 3 (List, Cancel, Retry) |
| `beads` | 1 (Reconcile) |
| `inbox` | 1 (List) |
| `logs` | 1 (Tail) |

Registered onto the clap tree at `.../crates/ainb-core/src/cli/registry.rs:3126-3160`; dispatch at `registry.rs:3159-3160` → `crate::cli::hangar::dispatch`.

**Migrations**: **93** (`0001_init_workspace_user_member.sql` … `0093_board_card_issue_index.sql`), in `.../crates/ainb-hangar-store/migrations/`, creating **63 tables**:

`activity_log, agent, agent_invocation_target, agent_runtime, agent_skill, agent_task_queue, atc_instance, atc_retry, attention, autopilot, autopilot_collaborator, autopilot_rule_version, autopilot_run, autopilot_run_copy, autopilot_subscriber, autopilot_webhook_delivery, beads_mapping, board, board_card, board_column, card_dependency, comment, daemon_config, daemon_socket_token, daemon_token, dispatch_attempt, event_log, fleet_acp_session, fleet_action_receipt, fleet_activity, fleet_channel, fleet_channel_member, fleet_confirm, fleet_event, fleet_message, fleet_message_delivery, fleet_provider_event, fleet_session, fleet_work_item, fleet_work_item_next, inbox_entry, interactive_codex_thread, issue, issue_cascade_barrier, issue_label, issue_property, issue_reaction, issue_subscriber, label, member, notify_rule, pat, profile, run_history, skill, skill_file, squad, squad_member, standup_session, task_usage, user, workspace, workspace_invitation`

**LOC** (`src/**/*.rs` vs `tests/**/*.rs`):

| Crate | src | files | tests | files |
|---|---:|---:|---:|---:|
| ainb-hangar-daemon | 72,454 | 64 | 55,886 | 151 |
| ainb-plugin-hangar | 56,723 | 55 | 16,924 | 47 |
| ainb-hangar-store | 40,185 | 66 | 27,711 | 84 |
| ainb-hangar-proto | 12,883 | 11 | 1,029 | 4 |
| ainb-hangar-core | 9,175 | 36 | 602 | 5 |
| ainb-fleet-core | 8,902 | 26 | 818 | 1 |
| ainb-acp | 2,887 | 7 | 1,131 | 5 |
| ainb-hangar-client | 976 | 2 | 0 | 0 |
| ainb-hangar-sandbox | 852 | 4 | 318 | 2 |
| ainb-hangar-secrets | 371 | 4 | 225 | 2 |
| **Total** | **205,408** | **275** | **104,644** | **301** |

Workspace-wide `src` total across all crates: 489,710 lines.

**Tripwires**: **170 files** named `tripwire*.rs` under `tests/`, containing **274 `#[test]`/`#[tokio::test]` fns**. 62 of those files are in `.../crates/ainb-hangar-daemon/tests/` (which holds 149 test files total).

**Source files >3000 lines** (hangar surface):

| Lines | File |
|---:|---|
| 16,706 | `.../crates/ainb-hangar-daemon/src/rpc/mod.rs` |
| 9,204 | `.../crates/ainb-plugin-hangar/src/plugin.rs` |
| 6,367 | `.../crates/ainb-plugin-hangar/src/screen/fleet.rs` |
| 5,783 | `.../crates/ainb-hangar-daemon/src/fleet.rs` |
| 5,121 | `.../crates/ainb-hangar-proto/src/snapshots.rs` |
| 5,034 | `.../crates/ainb-plugin-hangar/src/screen/issue_list.rs` |
| 4,868 | `.../crates/ainb-hangar-daemon/src/run_loop.rs` |
| 4,470 | `.../crates/ainb-plugin-hangar/src/screen/boards.rs` |
| 4,324 | `.../crates/ainb-hangar-daemon/src/rpc/snapshots.rs` |
| 3,701 | `.../crates/ainb-hangar-daemon/src/fleet_provider/codex_manager.rs` |
| 3,310 | `.../crates/ainb-plugin-hangar/src/screen/app_screens.rs` |
| 3,230 | `.../crates/ainb-hangar-daemon/src/acp_pool.rs` |
| 3,068 | `.../crates/ainb-hangar-store/src/repo/fleet.rs` |

Outside the hangar crates but on the same operating surface:

| Lines | File |
|---:|---|
| 16,175 | `.../crates/ainb-core/src/cli/hangar/mod.rs` |
| 14,666 | `.../crates/ainb-core/src/app/state.rs` |
| 10,689 | `.../crates/ainb-core/src/app/events.rs` |
| 4,458 | `.../crates/ainb-core/src/interactive/session_manager.rs` |
| 4,041 | `.../crates/ainb-core/src/config/mod.rs` |
| 3,759 | `.../crates/ainb-core/src/cli/fleet/atc.rs` |
| 3,561 | `.../crates/ainb-core/src/cli/registry.rs` |

Other large files: `.../crates/ainb-hangar-daemon/src/runner.rs` 2421, `.../crates/ainb-hangar-daemon/src/attention_ingest.rs` 2116, `.../crates/ainb-hangar-proto/src/methods.rs` 2180, `.../crates/ainb-hangar-daemon/src/answer.rs` 1461, `.../crates/ainb-plugin-hangar/src/screen/fleet_chat.rs` 2901, `.../crates/ainb-hangar-daemon/src/board.rs` 688.

---

## 3. Seams that make the devX brittle (observed, not opinion)

### 3.1 Terminal key-driving instead of a protocol

- **103 `capture-pane` / `send-keys` / `has-session` / `list-sessions` call sites** in `src/` (excluding test modules) across `ainb-fleet-core`, `ainb-hangar-daemon`, `ainb-plugin-hangar`, `ainb-core`. Concentration: `.../crates/ainb-fleet-core/src/fleet/send/tmux.rs` (17), `.../crates/ainb-fleet-core/src/fleet/send/route.rs` (8), `.../crates/ainb-fleet-core/src/fleet/read/tmux_pane.rs` (5), `.../crates/ainb-core/src/tmux/process_detection.rs` (4), `.../crates/ainb-core/src/fleet/bridge/transport.rs` (4).
- **The scraping is in the DAEMON, not the TUI.** The TUI plugin sends a structured `attention/answer` / `fleet/action` RPC over the unix socket; `answer.rs:218` (`deliver`) then reads the pane and drives keys. The plugin's own tmux use is limited to attaching a popup (`.../crates/ainb-plugin-hangar/src/plugin.rs:586` `launch_fleet_tmux_popup`, `:602` `fleet_tmux_attach_command`, applied at `:2887`) and rendering a copyable `tmux attach -t <name>` hint (`plugin.rs:3239-3254`). It never scrapes.
- **The scrape is load-bearing for correctness, not cosmetics.** `route_answer` (`answer.rs:176`) refuses delivery in 4 distinct pane conditions (listed in §1e). A pane the daemon cannot read while the row says a picker is up leaves the row open rather than typing blind (`answer.rs:232-239`).
- **Interactive tasks have no capture at all.** `run_interactive` (`run_loop.rs:1559`) produces no stream-json; the tmux pane is the sole record. `structured` is `false` for every interactive spec (`runner.rs:1327`), so the finalize cannot use the provider's own reported outcome and falls back to session-reaped detection.
- Structured alternatives exist in-repo for exactly two providers (ACP `rpc/mod.rs:6088`; codex app-server `rpc/mod.rs:4663`), and **neither is reachable from a task**.
- The tmux module's own header (`tmux.rs:1-60`) documents that the send path had to be re-derived from PTY read-size measurements, that no fixed Enter delay is safe, and that verification cannot look at the payload because the 80x24 composer is tail-anchored. That is the cost surface of not having a protocol.

### 3.2 State duplicated across issue / card / task

Four independent state stores for one unit of work:

| Store | Column | Vocabulary |
|---|---|---|
| `issue` | `state` | `backlog`/`todo`/`in_progress`/`in_review`/`done`/`blocked`/`cancelled`, **free TEXT, no CHECK** (`migrations/0023_issue_lifecycle_vocab.sql:20`, `0049_issue_state_blocked_cancelled.sql`) |
| `board_card` | `column_id` | per-board columns; position on a board (`0027_board.sql`, `0034_board_card_ord.sql`) |
| `agent_task_queue` | `status` | CHECK'd 6-state FSM (`migrations/0004_agent_task_queue.sql:28`) |
| in-process | `LifecycleGuard` | typed FSM re-asserting the same edges (`run_loop.rs:1236`, `crate::fsm`) |

Synchronisation is a fan of best-effort, advance-only, never-blocking writes, all in `.../crates/ainb-hangar-daemon/src/board.rs`:
`auto_move_after_transition:44`, `advance_issue_lifecycle_after_transition:126`, `advance_issue_lifecycle_after_terminal:225`, `auto_move_after_terminal:301`, `unblock_dependents_after_terminal:409`, `record_task_outcome:502`, `advance_and_cascade_child:537`, `maybe_cascade_child_done:571`, `deliver_cascade:616`.

Every one logs-and-swallows on fault (e.g. `board.rs:150-153`, `board.rs:197-199`), so divergence is silent by construction. The code names the twinning explicitly: "Twin the durable-card move on the issue's own `state`" (`run_loop.rs:1813-1816`).

Known asymmetries pinned in comments:
- The default issue board buckets by `issue.state`, not the durable `board_card` the auto-move touches, so both must be written or "a plain task's card strands in Todo through its whole run" (`run_loop.rs:1300-1305`).
- A finished pipeline STAGE is not a finished ISSUE: promoting on one stage's `done` aggregate marked a card `done` with Review and QA still to go (`board.rs:215-219`).
- Migration 0078 added `board_column_id` to the task row so the pull SQL could answer "is this stage already finished"; the generation alone was not enough (`pull.rs:59-70`).
- Advance ranking must use `advance_rank`, not board column `order()`, because `blocked` paints at column 5 right of `done` and column order would freeze a blocked issue forever (`board.rs:115-119`).
- Migration 0023 rewrote legacy `open`→`todo` / `closed`→`done` in place, and deliberately leaves unknown tokens as-is, relying on the display layer to bucket them under Todo (`0023…sql:16-20`).

A fifth state exists at the fleet layer: `fleet_session` carries `lifecycle_state` / `attention_state` / `management_state` / `transport_health` / `confidence` (`rpc/mod.rs:5970-5976`), and `fleet_acp_session` carries its own `state` (`rpc/mod.rs:5987`).

### 3.3 CLI writes the store directly, bypassing the daemon

`.../crates/ainb-core/src/cli/hangar/mod.rs` opens SQLite directly: **18 `Store::open_default()` calls**, **34 `sqlx::` uses**, and **ZERO** references to `ainb_hangar_client`, `HangarClient`, or any RPC call (`rg -c 'HangarClient|rpc_call|call_daemon|hangar_client'` → 0). The code says so in its own doc comments: "workspace-scoped and daemon-less" (`cli/hangar/mod.rs:5824`).

Consequence, confirmed: every `hangar issue …` mutation goes through `run_issue_*(&store, …)` (`cli/hangar/mod.rs:5799-5820`) and emits **no `HangarEvent`**: `rg -c 'EventLogRepo|event_log'` in that file returns 0. The daemon's `EventSink` is the only producer of the pushes the TUI subscribes to (`emit_task_started` `run_loop.rs:2246`, `emit_task_finished` `run_loop.rs:2270`, `events.emit_attention` `atc.rs:131`).

`ainb-hangar-client` (976 LOC) is consumed only by `.../crates/ainb-fleet-tools/src/fleet.rs`, `.../crates/ainb-fleet-tools/src/keyfile.rs`, `.../crates/ainb-core/src/fleet/bridge/daemon.rs`, and one daemon live test: never by the hangar CLI.

The TUI plugin, by contrast, dials `{hangar_home}/hangar.sock` via the host `unix_socket_dial` capability and speaks framed JSON-RPC (`.../crates/ainb-plugin-hangar/src/plugin.rs:3-20`, `daemon_socket_path:80`, auth-first-frame note `:88-95`, `.../crates/ainb-plugin-hangar/src/jsonrpc_over_socket.rs`, `.../crates/ainb-plugin-hangar/src/connection.rs`).

So there are **two disjoint write paths into one database**: daemon (events, subscribers, FSM services) and CLI (raw sqlx, silent).

### 3.4 Duplicated concepts

Nine distinct row-level concepts for "a thing an agent is doing", each with its own table(s) and RPC family:

| Concept | Table(s) | RPC family |
|---|---|---|
| issue | `issue`, `issue_property`, `issue_label`, `issue_subscriber`, `issue_reaction`, `issue_cascade_barrier` | `hangar/issue_*` (28) |
| board card | `board_card`, `board_column`, `card_dependency` | `hangar/board_*` (19) |
| task | `agent_task_queue` | `hangar/task_*`, `hangar/*_run` |
| dispatch attempt | `dispatch_attempt` (0058) | `hangar/dispatch_attempts_list` |
| run | `run_history` (0029), `task_usage` (0022) | `hangar/run_history`, `hangar/usage_rollup` |
| fleet session | `fleet_session`, `fleet_event`, `fleet_provider_event`, `fleet_action_receipt` (0044, 0071) | `fleet/*` (33) |
| ACP session | `fleet_acp_session` (0082) | `fleet/acp_session_create` |
| codex thread | `interactive_codex_thread` (0086, 0087, 0088, 0089, 0090, 0091: **six migrations**) | `codex/session_ensure`, `codex/session_discard` |
| attention row | `attention` (0025, 0026, 0084) | `attention/*` (3) |

Two parallel "a human must answer" mechanisms coexist: `attention` (six kinds, answered via `attention/answer` → tmux screen-scrape) and `fleet_confirm` (0044, answered via `fleet/confirm_answer`), with `fleet_action_receipt` as a third record of the same interaction.

Two parallel transcript stores: `{logs}/<provider>.jsonl` on disk for tasks, parsed CLIENT-SIDE by `.../crates/ainb-plugin-hangar/src/widgets/jsonl_timeline.rs`; and `fleet_provider_event` rows in SQLite for fleet/ACP sessions, written by `.../crates/ainb-acp/src/store_writer.rs`. The plugin renders both into the same `ViewEntry` shape (`jsonl_timeline.rs:1-11`).

Three "session id" notions on one task: `agent_task_queue.session_id` (the provider's own id, pinned from stream-json), `session_name` (the tmux session for interactive), and `fleet_session.session_key` (the fleet-layer identity used by attention answering).

### 3.5 Provider-specific branching

**54 `Backend::{Claude,Codex,Copilot,Antigravity}` occurrences** in src: `run_loop.rs` 24, `runner.rs` 20, `claude_cred.rs` 7, remainder elsewhere. Four `*_spec` builders (`runner.rs:1295 / 1368 / 1468 / 1495`) plus a 4-arm dispatch match (`run_loop.rs:1405-1449`), each arm with a different signature (`run_claude_in_with_env` takes `cred_env`; the other three take `dispatch.agent_env.expose_for_child_env()`: "the ONE permitted plaintext escape").

`AgentKind` (`.../crates/ainb-hangar-core/src/agent_kind.rs:35-46`) mirrors the same four. Its doc comment says Copilot "Selectable in the picker; dispatch returns a clear 'provider not yet wired' error until the third runner backend lands" (`agent_kind.rs:42-45`), while `run_loop.rs:1427` DOES route `Backend::Copilot` to `run_copilot_in` and `copilot_spec` exists at `runner.rs:1468`. **Doc and code disagree; I did not verify which is current** (inferred divergence, not confirmed behaviour).

Provider divergence extends well past argv:
- codex needs `exec --skip-git-repo-check -s danger-full-access --json` headless but NONE of them interactively (`runner.rs:1338-1345`), because `--skip-git-repo-check` is an `exec`-only flag and a top-level parse error.
- codex carries a retired-model substitution at the wire (`runner.rs:1385-1394`).
- claude requires `--verbose` whenever `--print` + stream-json are combined (`runner.rs:1306-1312`).
- copilot requires `--allow-all-tools` for non-interactive mode (`runner.rs:1291-1294`).
- Four separate log filenames (`runner.rs:1323, 1403, 1482, 1518`).
- The client-side jsonl parser handles claude's taxonomy "fully" and codex's `{"msg":{...}}` envelope "on a best-effort basis"; copilot and antigravity shapes are unhandled and silently skipped (`jsonl_timeline.rs:22-27`).
- `claude_cred.rs` has 7 Backend branches of its own.
- `AINB_CODEX_BIN`, `AINB_CODEX_MANAGED`, `AINB_CODEX_APP_SERVER`, `AINB_TEST_CODEX_BINARY`, `CODEX_HOME`: five of the 30 daemon env vars are codex-specific.

Separately, `fleet/action` fans to **three** transports by provider token (`rpc/mod.rs:4045-4075`): ACP (`execute_acp_action`), codex-managed (`execute_codex_action`, with a tmux fallback when the transport is inactive), and a tmux fallthrough (`verified_tmux_picker:5649` / `verified_tmux_send:5609`).

A frozen `argv_golden_matrix` contract test exists to pin per-provider argv (`.../crates/ainb-hangar-daemon/tests/`, described in `ainb-tui/CLAUDE.md`), which is itself evidence of how much per-provider surface there is to freeze.

### 3.6 Environment configuration

**30 distinct env var names** read in `.../crates/ainb-hangar-daemon/src` (78 `env::var` call sites across all hangar crates; 57 distinct `HANGAR_*`/`AINB_*` string literals appear in src overall, so roughly 27 are referenced somewhere but not read in the daemon).

```
AINB_ACP_TURN_DEADLINE_MS   AINB_BIN                      AINB_CODEX_APP_SERVER
AINB_CODEX_BIN              AINB_CODEX_MANAGED            AINB_FLEET_DISABLE_TMUX_DISCOVERY
AINB_FLEET_USAGE_MIN_GAP_MS AINB_HANGAR_BOOT_TASK_ID      AINB_HANGAR_OWNERSHIP_WATCH_MS
AINB_HANGAR_WEBHOOK_PORT    AINB_HOME                     AINB_TEST_CODEX_BINARY
BEADS_DIR                   CODEX_HOME                    HANGAR_ANTIGRAVITY_PATH
HANGAR_CLAUDE_PATH          HANGAR_CODEX_PATH             HANGAR_COPILOT_PATH
HANGAR_DAEMON_DISABLE_CLAIM HANGAR_DAEMON_DISABLE_SANDBOX HANGAR_DAEMON_POLL_MS
HANGAR_GC_INTERVAL_MS       HANGAR_KEEP_FAILED_RUNS       HANGAR_PRESENCE_SWEEP_MS
HANGAR_PROVIDER_MAX_RUNTIME_MS  HANGAR_SPAWN_SETUP_TIMEOUT_MS
HANGAR_SWEEP_DISPATCHED_TTL_MS  HANGAR_SWEEP_INTERVAL_MS
HOME                        OTEL_EXPORTER_OTLP_ENDPOINT
```

Plus, read outside the daemon crate: `HANGAR_DAEMON_RUNTIME_ID` (`.../crates/ainb-hangar-store/src/bootstrap.rs`), `HANGAR_CLAUDE_OAUTH_TOKEN` (`.../crates/ainb-hangar-daemon/src/claude_cred.rs`), `AINB_HANGAR_HOME` (path resolution, `.../crates/ainb-hangar-core/src/paths.rs`: 3 env reads), `TMUX` (`.../crates/ainb-plugin-hangar/src/plugin.rs`), `FAKE_ACP_ENV_DUMP` (`.../crates/ainb-acp/src/bin/fake_acp_adapter.rs`), `PATH` (`.../crates/ainb-hangar-sandbox/src/lib.rs`).

There is **no single registry** of these in code. `DaemonConfig::from_env` (`run_loop.rs:185-268`) covers 12; the rest are read at their point of use, via three different helpers (`std::env::var_os`, `env_u64` at `run_loop.rs:353`, `env_u64_opt` at `:358`). Only a subset is documented in `ainb-tui/CLAUDE.md` (the credential ladder and `HANGAR_CLAUDE_OAUTH_TOKEN`). A sandbox override that is neither `"0"` nor `"1"` silently falls back to the platform default (`run_loop.rs:278-284`) rather than erroring.

### 3.7 Seams and pinch points worth naming [Feathers]

Genuine seams (behaviour substitutable without editing in place):
- `SecretBackend` trait object: `execute_claimed` takes it as `Arc<dyn ...>`, so a test injects an in-memory double instead of touching the real Keychain (`run_loop.rs:545-549`, `:1095`).
- `HangarClock` / `IdGen` trait objects threaded through every service (`run_loop.rs:1091`, `pull.rs` signatures).
- `interactive_command` (`run_loop.rs:1538`): the one place interactive argv is derived, reachable from a test without tmux, a provider binary, or a database. Extracted precisely because the previous shape let a test re-derive the argv *beside* the call rather than through it.
- `route_answer` (`answer.rs:176`): pure function over `(pane, picker, answer)`, testable from fixtures.
- `DaemonConfig::resolve_sandbox` (`run_loop.rs:278`): pure, so override precedence is testable without mutating process env.
- `ainb-acp` as a whole: pure library, sqlx-fenced by test, promotable to a standalone process without redesign (`lib.rs:17-24`).
- `PULL_SQL` as a string const (`pull.rs:531`): enables the clause-deletion mutation proofs.

Pinch points (narrow interfaces where one characterization test covers a lot):
- `execute_claimed` (`run_loop.rs:1087`): every task, every provider, every mode passes through it.
- `run_card` (`rpc/mod.rs:10148`): every launch path (issue_run, board_card_run, autopilot, auto-run) converges here.
- `answer` (`answer.rs:66`): every surface (TUI, web, bridge, ATC) answers through it.
- `build_prompt` (`run_loop.rs:2548`): the single brief composer.

Where behaviour is unverifiable without a harness:
- The interactive tmux path has no structured output; the module comment itself notes the only coverage was "a tmux tripwire that SKIPs when tmux is absent" (`run_loop.rs:1536-1537`).
- The tmux send timing behaviour is pinned to measured fixtures in `pane_fixtures/` against specific claude/tmux/macOS versions (`tmux.rs:13-16`); a provider UI change invalidates them silently.
- The sandbox returns `Enforcement::None` on unsupported platforms and the confinement test SKIPs rather than asserting (`.../crates/ainb-hangar-sandbox/src/lib.rs:31-35`): correct, but it means CI on an unsupported platform proves nothing about confinement.

---

## Confidence

**Confirmed by reading the code path:** the whole task execution flow (§1a–c); ACP's absence from the task path (zero-match grep on both `run_loop.rs` and `runner.rs`); ACP's fleet-session-only role and its two adapters; the tmux screen-scrape HITL path and its four refusal cases; the absence of any `--input-format` / control-request / SDK channel (zero-match repo-wide grep); the CLI's daemon-less direct-store access (18 `Store::open_default`, 0 client references); every count in §2, each from a command whose output was inspected.

**Inferred, not verified by running:** the Copilot doc/code divergence in §3.5; the claim that CLI mutations produce no TUI update (I confirmed no event emission in the CLI file and that `EventSink` lives in the daemon, but did not run the pair and observe a stale TUI); the ~27 referenced-but-not-read env vars (derived from two greps, not from tracing each); the per-group leaf-count breakdown in the CLI table (derived from an AST-ish regex over the enum bodies, not from `--help` output).

**Not examined:** `.../crates/ainb-plugin-hangar/src/screen/fleet.rs` (6,367 lines) and `.../crates/ainb-hangar-daemon/src/fleet.rs` (5,783 lines) internals; the autopilot scheduler and its cron/webhook surface; beads sync; the web crate; whether any tripwire exercises the interactive tmux path end-to-end under CI conditions.

**Open questions a maintainer must answer:**
1. Is `Backend::Copilot` dispatchable today, or does `agent_kind.rs:42-45` still hold? The two disagree.
2. Is there any mechanism that reconciles `issue.state` against `board_card.column_id` after a swallowed best-effort write, or does divergence persist until a human moves the card?
3. Do the 62 daemon tripwires run in CI? `default-members = ["crates/ainb-core", "crates/ainb-hangar-daemon"]` includes the daemon, so presumably yes: but `ainb-hangar-store` (84 test files) and `ainb-plugin-hangar` (47) are NOT in default-members, and per `ainb-tui/CLAUDE.md` a plain `cargo nextest run` silently skips them while still reporting a clean summary.
4. Do `hangar issue`/`hangar task` CLI mutations need to notify a running daemon, or is the silent-write behaviour intentional? Nothing in the code marks it either way beyond the "daemon-less" comment.
