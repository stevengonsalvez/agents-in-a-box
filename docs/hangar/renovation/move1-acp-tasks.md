> Appendix to `docs/hangar/renovation/PLAN.md`. Planning pass output, unedited except for punctuation.

# Move 1: two first-class task executors (process | acp), one live run-event stream

Base: `/home/claude/.agents-in-a-box/worktrees/by-name/agents-in-a-box--f-prove-hangar--22bb34f4/ainb-tui/`
Every `file:line` verified by reading the file at HEAD of `f/prove-hangar`.
Rev 2: folds in "both executors stream live", process path stays first class.

---

## 0. Recommendation, up front

1. **No `TaskExecutor` trait.** The second executor already exists here and it is a
   branch: `run_loop.rs:1387-1449` is `if mode == Mode::Interactive { run_interactive } else
   { match backend }`, both arms returning `anyhow::Result<RunOutcome>`. A third arm is the
   same seam for less code. Extract a trait when there is a fourth.
2. **No new event type and no new stream.** `HangarEvent::TaskMessage` and
   `HangarEvent::TaskProgress` already exist, are already routed by the outbox
   (`event_outbox.rs:69-74`), already de-prioritised by the inbox aggregator
   (`inbox_aggregator.rs:126-127`), and already have plugin CONSUMERS
   (`plugin.rs:1197`, `banner_state.rs:135`). **Neither has a producer anywhere in `src/`.**
   Move 1 writes the producers. That is the whole "one run-event stream".
3. **Do not dual-write process stdout into `fleet_provider_event`.** `store_writer.rs:14`
   says it plainly: every commit takes the single SQLite write lock the whole control plane
   shares, and ACP is already "the new heavy writer". A claude run emits thousands of
   stream-json lines. Each executor keeps its own durable store; the READ is unified
   server-side, once.
4. **Move the transcript classifier into `ainb-hangar-proto`** so the daemon and the plugin
   share one taxonomy. It is a move of an existing 498-line file, not new code, and it is
   what makes both producers honest.
5. **No turn-complete future on the pool.** Poll the delivery leg the pool already resolves
   on every path.
6. **Per-task adapter process for ACP**, not the shared pool. Reasoning in section 5.

---

## 1. What the AcpPool gives a caller today, and what a TASK caller is missing

### 1a. Session creation

`fleet/acp_session_create` (`rpc/mod.rs:5907-6010`) mints the `fleet_session` +
`fleet_acp_session` PAIR in one transaction, with **no process spawn**:

| Step | Location |
|---|---|
| capability gate `FLEET_CAPABILITY_ACP_SPAWN` | `rpc/mod.rs:5918` |
| adapter-registry validation (not schema) | `rpc/mod.rs:5929-5946` |
| `permission_mode` read from `PoolConfig` | `rpc/mod.rs:5942-5946` |
| `session_key` minted by `FleetAcpSessionRepo::mint_session_key` | `rpc/mod.rs:5948` |
| `scope_key` = param or `session:<session_key>` | `rpc/mod.rs:5949` |
| paired insert | `rpc/mod.rs:5979-5993` |
| idempotent **per live scope** | `rpc/mod.rs:5994-6005` |

`FleetAcpSessionRow` (`crates/ainb-hangar-store/src/repo/fleet_acp_session.rs:43-68`):
`session_key, scope_key, provider, provider_version, acp_session_id, cwd, permission_mode,
state, open_turn_id, open_turn_started_at, created_at, last_active_at`, plus migration
0082's `model / reasoning_effort / persona`.

**CONTRADICTS the brief.** The brief says "one process per provider means the sandbox and
cwd can't be per task". **cwd IS per session already**: a column on `fleet_acp_session`,
carried onto the actor (`acp_pool.rs:1528`), passed to `session/new` (`acp_pool.rs:2338-2341`)
and `session/load` (`acp_pool.rs:2280-2287`). What is genuinely per-PROCESS: the child
environment, the permission mode, and the OS sandbox. See section 5.

### 1b. Sending a prompt and observing turn completion

`AcpPool::submit_prompt(session_key, message_id, text) -> SubmitOutcome`
(`acp_pool.rs:608-651`), fire and forget: `Queued` or `Rejected(&'static str)`. No future,
no callback, no event addressed to the caller. The outcome lands in the store.

```
submit_prompt ──▶ actor queue (bounded, queue_depth=32)
                     │ one turn in flight per session (acp_pool.rs:1584)
                     ▼
                  start_turn (acp_pool.rs:1720)
                     │  ├─ leg already terminal? skip          (:1726)
                     │  ├─ attach_with_one_requeue             (:2102)
                     │  ├─ mode_violated? FAILED mode_unproven (:1772)
                     │  ├─ in-flight permit (max 4/process)    (:1784)
                     │  └─ record_turn (open_turn_id)          (:1853)
                     ▼
                  session/prompt on its own task               (:1831)
                     ▼
                  finish_turn (acp_pool.rs:1918)
                     └─ commit_turn_end ── ONE txn ──▶ receipt + reply + IDLE (:2038)
```

`message_id` must already carry a PENDING delivery leg; `start_turn` re-reads it
(`leg_is_pending` `acp_pool.rs:2874`) and drops the job otherwise. The chat path creates it
with `acp_operator_message` (`rpc/mod.rs:6285-6317`).

Leg row: `FleetMessageDeliveryRow` (`crates/ainb-hangar-store/src/repo/fleet_message.rs:73-86`),
`state` in `PENDING|DELIVERED|FAILED|UNKNOWN|REJECTED`, `detail` an enumerated token from
the taxonomy at `acp_pool.rs:100-141` (`queue_full, breaker_open, adapter_exit,
operator_stop, turn_deadline, daemon_restart, spawn_failed, turn_failed, turn_unrecorded,
session_gone, provider_at_capacity, mode_unproven`), plus `resume=loaded|reprimed`
appended (`acp_pool.rs:1959-1963`).

### 1c. Cancel

`AcpPool::cancel(session_key, ConvergeCause)` (`acp_pool.rs:683`), turn-scoped
`cancel_turn(..., Some(turn_id))` (`:692`), `teardown` (`:714`). `session/cancel` is a
notification (`client.rs:545`) so it never blocks behind the turn it kills.

### 1d. Resume

`ensure_session` (`acp_pool.rs:2165`) does not depend on `session/load`: stored id +
adapter advertises `loadSession` -> `try_load` (`:2259`); unknown session -> `rebuild`
(`:2329`) = `session/new` plus a re-prime prelude prepended to the next prompt
(`:1801-1804`), built by `render_resume_prelude` (`:2930`). Mode is re-asserted after every
load (`client.rs:459-496`) because both real adapters revert it.

### 1e. cwd / env / MCP / permission_mode

- **cwd**: per session.
- **env**: per PROCESS. `AdapterConfig { env_passthrough, extra_env }` (`config.rs:41-44`)
  through `allowlisted_env` (`config.rs:117-143`) over `BASE_ENV_ALLOWLIST = ["PATH","HOME"]`
  (`config.rs:22`), spawned `env_clear()` (`client.rs:684-701`).
- **MCP**: per SESSION, deliberately (`client.rs:436-440`), via
  `crate::copilot::session_mcp_servers(pool, scope_key)` (`acp_pool.rs:2278`, `:2336`).
- **permission_mode**: per PROCESS (`config.rs:37`). Valid values validated at
  `acp_pool.rs:331`: `default | acceptEdits | bypassPermissions | plan`.

  **CONTRADICTS the brief.** It is not "pinned at `session/new`": upstream v1
  `NewSessionRequest` has no mode field (`client.rs:34-43`). The mode is ASSERTED: read
  `currentModeId` off the reply, and on a mismatch issue `session/set_mode` and require a
  `current_mode_update` echo within 5 s (`client.rs:68`, `:612-665`). No echo is a hard
  spawn failure. There is no `ask` mode.

### 1f. request_permission park and answer

Parked in the client holding the adapter's `Responder` (`client.rs:211-301`). The pool
raises it (`raise_permission` `acp_pool.rs:2513`): refused at the door with no open turn
(`:2520-2523`); fingerprint `permission_fingerprint` (`:3154`); `acp.permission` transcript
row (`:2534`); `AttentionKind::Approval` with `session_id = session_key`, `cwd = actor cwd`,
`workspace_id: None` (`:2544-2556`); fleet event `acp_permission_requested` setting
`attention_state=APPROVAL` (`:2562-2581`).

Answered by `answer_permission(session_key, fingerprint, PermissionDecision)`
(`acp_pool.rs:654`) -> actor `answer` (`:2595`) -> `answer_selected`/`answer_cancelled`
(`client.rs:277-300`) -> `retire_attention` (`:2664`). Reachable from the wire ONLY through
`fleet/action` -> `execute_acp_action` (`rpc/mod.rs:6083-6200`).

### 1g. What is MISSING for a task caller

| Need | Status | Gap |
|---|---|---|
| per-worktree cwd | **present** | none |
| turn-complete signal | **missing** | poll the delivery leg (section 2d) |
| stop reason on a SUCCESS | **missing** | `finish_turn` records it only on failure (`acp_pool.rs:1936-1947`); `turn_succeeded` folds `EndTurn`/`MaxTokens`/`MaxTurnRequests` into one `DELIVERED` (`:3039-3044`). 3-line fix in 2e. |
| usage / cost | **missing** | reducer classifies `UsageUpdate` to `ChunkKind::Usage` (`reducer.rs:318-320`) and persists an `acp.usage` row, but nothing parses it into `ProviderUsage` (`runner.rs:420-427`) |
| PR URL capture | **missing** | today `parse_gh_pr_create_stdout(&result.stdout_tail)` (`run_loop.rs:1718`). ACP has no stdout tail: adapter stderr is INHERITED (`client.rs:690-697`), stdout is the JSON-RPC pipe. |
| task -> ACP session mapping | **missing** | no column either side. Use `scope_key = "task:<task_id>"`: create is already idempotent per live scope, so no new column and no new write. |
| turn deadline vs task runtime | **mismatch** | `turn_deadline` default 30 min (`acp_pool.rs:264`) vs `HANGAR_PROVIDER_MAX_RUNTIME_MS` |
| concurrency ceiling | **mismatch** | `max_in_flight_per_process: 4` (`:261`), `max_sessions_per_provider: 16` (`:260`). A per-task process removes both. |

---

## 2. The seam, and the one live run-event stream

### 2a. What "live" costs today: nothing, because nothing produces it

```
                            PRODUCER              CONSUMER
HangarEvent::TaskMessage    NONE                  plugin.rs:1197 -> boards.rs:803 (timeline overlay)
HangarEvent::TaskProgress   NONE                  banner_state.rs:135 (tool count + latest line)
logs/<p>.jsonl              runner.rs:1908        board_card_timeline (pull) -> parse_timeline (client)
fleet_provider_event        StoreWriter (ACP)     fleet/transcript_list -- NO plugin reader at all
```

Verified: `rg 'HangarEvent::TaskMessage' crates/ | grep -v tests/` returns only
`event_outbox.rs:72` (routing) and `inbox_aggregator.rs:127` (ignore); same for
`TaskProgress`. `rg 'TRANSCRIPT_LIST|transcript_list' crates/ainb-plugin-hangar/src`
returns nothing.

So the TUI's live task transcript, the board timeline's live append (`plugin.rs:1177`'s
whole comment) and the run banner are all **dead code waiting for a producer**. Move 1
writes it, once, for both executors. No new variant, no new RPC, no new subscription.

### 2b. The shared classifier (the one refactor)

Both producers need `line -> Vec<(MessageKind, String)>`. That function exists:
`crates/ainb-plugin-hangar/src/widgets/jsonl_timeline.rs` (498 lines, 6 tests). It is in the
PLUGIN crate and returns the plugin type `ViewEntry`, so the daemon cannot use it, and
`ViewEntry::line(kind, body)` (`screen/task_detail.rs`) shows it is `(MessageKind, String)`
in a wrapper.

**Move it to `ainb-hangar-proto` as `transcript::classify`.** `MessageKind` already lives
there (`events.rs`), and both the daemon and the plugin already depend on that crate
(`crates/ainb-plugin-hangar/Cargo.toml:28`). It cannot live in `ainb-hangar-core`:
`ainb-hangar-proto` depends on core (`crates/ainb-hangar-proto/Cargo.toml:22`), not the
other way round, so core cannot see `MessageKind`.

```
ainb-hangar-proto::transcript
    classify_stream_json(line: &str) -> Vec<(MessageKind, String)>   // the moved parser
    classify_acp(event_type: &str, raw_payload: &str) -> Vec<(MessageKind, String)>  // new, ~40 lines
```

The plugin keeps a 5-line wrapper mapping to `ViewEntry::line`. The 6 tests move with it.
`ViewEntry`, the collapse grouping, and the renderer all stay in the plugin: only
classification moves.

`classify_acp` maps the seven `ChunkKind::event_type()` tokens (`reducer.rs:60-70`):

| `event_type` | `MessageKind` |
|---|---|
| `acp.message` | agent prose |
| `acp.user_message` | user/comment lane |
| `acp.thought` | thinking |
| `acp.tool_call` | tool call, name + compact input from the verbatim `block` |
| `acp.plan` | tool call lane |
| `acp.permission` | tool call lane, `[approval] <tool>` |
| `acp.usage` | folded into the run-status line, not a transcript row |
| `acp.transcript_truncated` (`store_writer.rs:71`) | error lane |

Rejected alternative: synthesising fake claude stream-json out of `acp.*` rows so the
existing client parser eats it. That is clever, not boring: two shapes to keep in sync and
a round trip through a format neither side speaks.

### 2c. Both producers

```
   ProcessExecutor                                       AcpExecutor
   runner.rs stream_stdout:1907                          acp_pool.rs ingest:1678
        │ writeln! logs/<p>.jsonl  (durable, unchanged)       │ StoreWriter -> fleet_provider_event
        │                                                     │ (durable, unchanged)
        └──▶ classify_stream_json ──┐         ┌── classify_acp ┘
                                    ▼         ▼
                    EventSink::emit(TaskMessage{task_id, kind, body})     per line/chunk
                    EventSink::emit(TaskProgress{task_id, tool_calls, elapsed_ms})  throttled 1/s
                                    │
                    plugin: fold_timeline_message  plugin.rs:1197   ✅ exists
                            banner_state.rs:135                     ✅ exists
                            task_detail append                      ← 1 new arm (~10 lines)
```

**Process side.** `stream_stdout` (`runner.rs:1894-1960`) is a single clean loop with one
`writeln!` and one `push_tail`. Add an optional `sink: Option<TaskLineSink>` to
`RunnerConfig` (constructed once at `run_loop.rs:530-533`, so it threads with no new
plumbing) and call it per line. `tool_calls` counts lines whose parsed `StreamLine` shows a
tool_use content block; claude's shape is exact, codex is best-effort, copilot and
antigravity count 0 -- **the same coverage the plugin's parser already documents**
(`jsonl_timeline.rs:22-27`). One `ponytail:` comment naming that ceiling.

**ACP side.** The actor's `ingest` (`acp_pool.rs:1678`) already receives every chunk before
handing it to the writer. Emit there. The actor holds `pool.events` (the same `EventSink`,
`acp_pool.rs:562`) so there is nothing to thread. The task's `task_id` comes from the
`scope_key = task:<task_id>` the executor created the session under.

**Throttle.** `TaskProgress` on a 1 s tick (the ACP actor already has one:
`WriterConfig::flush_interval`, `acp_pool.rs:1556`). `TaskMessage` per line, deliberately
unthrottled, because `plugin.rs:1177` exists precisely so a transcript line does NOT arm a
snapshot re-pull. Do not invert that.

### 2d. Executor selection

```
┌────────────────┐  agent.provider ends in "-acp"?  ┌──────────────┐
│ resolve_dispatch│─────────── else ────────────────▶│ Executor     │
│ run_loop:1160   │  HANGAR_TASK_EXECUTOR fallback   │ Process | Acp│
└────────────────┘                                   └──────┬───────┘
              ┌──────────────────────────────────────────────┴────────┐
   Process ───▼────────────────┐                     Acp ─────────────▼──────────┐
   │ mode==Interactive?        │                     │ acp_task::run_acp         │
   │  yes -> run_interactive   │                     │ no tmux, no argv, no cred │
   │  no  -> match backend     │                     └───────────────────────────┘
   └───────────────────────────┘
```

- **Per agent (durable):** reuse the existing `agent.provider` column
  (`crates/ainb-hangar-store/src/repo/agent.rs:107`) with the values `claude-agent-acp` /
  `codex-acp`, which are already the adapter registry tokens (`config.rs:11-13`).
  **No migration.**
- **Per task (resolved):** `ResolvedDispatch` gains `executor: Executor`
  (`run_loop.rs:2413-2436`), beside `mode`, for exactly the reason the doc comment there
  gives: it lives on the dispatch so the exec-path branch and the argv cannot disagree.
- **Daemon default:** `HANGAR_TASK_EXECUTOR=acp|process`, **default `process`**, read in
  `DaemonConfig::from_env` (`run_loop.rs:185-268`) through a pure resolver shaped exactly
  like `resolve_sandbox` (`run_loop.rs:278-284`) so precedence is testable without mutating
  process env. Unrecognised value falls back to `process` with a warning, matching the
  sandbox override's documented behaviour.
- Precedence: agent > daemon default.
- A true per-task override that differs from its agent needs a column on
  `agent_task_queue`. **Deferred**, not designed around. Do NOT overload
  `agent_task_queue.mode` (`""|headless|interactive`, validated `rpc/mod.rs:9661-9669`):
  executor and interactivity are two axes and conflating them is the bug
  `interactive_command` (`run_loop.rs:1538`) was extracted to prevent.

### 2e. The branch, and `run_acp`

Inside the `provider_run` async block (`run_loop.rs:1387`), one arm added above the
existing two, nothing below `let outcome = ...` touched:

```rust
let provider_run = async {
    if dispatch.executor == Executor::Acp {
        crate::acp_task::run_acp(pool, &task, &dispatch, &location, task_env).await
    } else if mode == Mode::Interactive {
        run_interactive(...).await          // unchanged
    } else {
        match dispatch.backend { ... }      // unchanged
    }
};
```

The cancel arm (`run_loop.rs:1452-1469`) gains one case: dropping the future does not
cancel an ACP turn, so it calls `pool.cancel(&session_key, ConvergeCause::OperatorStop)` --
the exact analogue of the interactive arm's `kill_session`.

`crates/ainb-hangar-daemon/src/acp_task.rs`:

```
1. pool = acp_pool::active_handle().await            (acp_pool.rs:1301)  None -> Failed{SpawnError}
2. session_key = acp_session::ensure(provider, cwd = location.cwd, scope_key = "task:<id>")
      Extracted from handle_fleet_acp_session_create's body (rpc/mod.rs:5979) so the RPC and
      this share ONE transaction. One function, two callers.
3. write session_key onto agent_task_queue.session_id
4. message_id = acp_message::enqueue(session_key, prompt, sender = "task:<id>")
      Extracted from acp_operator_message (rpc/mod.rs:6285) the same way.
5. pool.submit_prompt(...).await;  Rejected(detail) -> map and return
6. poll the leg every 1 s (FleetMessageRepo::deliveries_for_message) while PENDING.
   Bounded by HANGAR_PROVIDER_MAX_RUNTIME_MS; on breach cancel(TurnDeadline) -> Failed{Timeout}.
   It also terminates on its own: the pool's deadline sweep resolves the leg (acp_pool.rs:885, :264).
7. build RunnerResult from the transcript, map the leg -> RunOutcome.
```

Why poll rather than add a `oneshot` to `PromptJob`: the leg is already resolved on every
path in one transaction (`commit_turn_end` `:2038`, `resolve_leg` `:2986`, convergence
`drain_queue` `:2471`). A oneshot is ~40 lines and one missed send leaves the task frozen
`running` until the multi-hour TTL, which is the exact hazard `run_loop.rs:1280-1294`
documents. Poll cost is one indexed read per second per running ACP task, capped by
`max_concurrent_tasks`.

### 2f. RunOutcome mapping

Needs one 3-line pool change so a success carries its stop reason: at `acp_pool.rs:1936-1947`
the success arm passes `detail = None`; make it `Some(format!("stop={:?}",
response.stop_reason))`. Additive -- the detail already gets `resume=...` appended the same
way (`:1959-1963`) and no reader parses it strictly.

| leg `state` | leg `detail` | `RunOutcome` | retry disposition |
|---|---|---|---|
| `DELIVERED` | `stop=EndTurn` | `Success(result)` | n/a |
| `DELIVERED` | `stop=MaxTokens` / `stop=MaxTurnRequests` | `Failed{IterationLimit}` | FreshRetry |
| `FAILED` | `turn_failed; Refusal` | `Failed{AgentError}` | NoRetry |
| `FAILED` | `turn_failed; Cancelled` | `Cancelled(result)` | n/a |
| `FAILED` | `spawn_failed` / `mode_unproven` / `turn_unrecorded` | `Failed{SpawnError}` | NoRetry |
| `FAILED` | `session_gone` | `Failed{ProvisionError}` | NoRetry |
| `FAILED` | `breaker_open` / `provider_at_capacity` / `queue_full` | `Failed{RuntimeOffline}` | ResumeRetry |
| `FAILED` | `operator_stop` | `Cancelled(result)` | n/a |
| `UNKNOWN` | `adapter_exit; ...` | `Failed{RuntimeOffline}` | ResumeRetry |
| `UNKNOWN` | `turn_deadline` | `Failed{Timeout}` | NoRetry |
| `UNKNOWN` | `daemon_restart` | `Failed{RuntimeRecovery}` | ResumeRetry |
| `REJECTED` | any | `Failed{SpawnError}` | NoRetry |
| (unmapped detail) | | `Failed{ProviderContractDrift}` | NoRetry |

Dispositions verified against `RetryService::retry_disposition`
(`crates/ainb-hangar-store/src/service/retry.rs:325-352`). The unmapped arm is
`ProviderContractDrift` deliberately, mirroring `runner.rs`'s existing fail-closed rule: an
unknown detail token means the pool's taxonomy grew, not that the agent succeeded.

`RunnerResult` for an ACP run: `exit_code: None`, `session_id: Some(session_key)`, `usage`
from the `acp.usage` rows, `stdout_tail` = the turn's final agent message plus tool-result
text (what PR capture scans), `stderr_tail: String::new()`.

---

## 3. Structured HITL

### 3a. ACP-only, and say so

**Structured HITL is ACP-only in Move 1.** The process executor keeps the existing hook and
attention route, unchanged, untouched.

State that route honestly, because it is narrower than it sounds:

```
executor=process, mode=interactive:
   claude hook ─▶ ainb fleet atc hook ─▶ events.jsonl ─▶ attention_ingest ─▶ attention row
   attention/answer ─▶ answer.rs:66 ─▶ capture_pane + tmux send-keys ─▶ the pane

executor=process, mode=headless:
   claude -p --dangerously-skip-permissions  (runner.rs:1296-1320)
   -> NOTHING EVER ASKS. There is no HITL on this path and never was.

executor=acp:
   session/request_permission ─▶ parked responder + attention row (acp_pool.rs:2513)
   answered ─▶ pool.answer_permission ─▶ the adapter's pending JSON-RPC id
```

So "the process path keeps the hook route" is true only for `mode=interactive`. The
headless process path has no human-in-the-loop at all, by argv. That asymmetry is the
reason ACP is worth doing, and it should be written down rather than implied.

### 3b. The gap the brief does not account for

The brief says permissions are answered "through the existing `fleet/action` path" and show
"in the TUI inbox/control center". Both are true separately and **do not connect**:

```
acp_pool::raise_permission ──▶ attention row (Approval)
              ┌───────────────────┴──────────────────┐
              ▼                                      ▼
  Control Center / Inbox                    Fleet screen
  attention/answer  (plugin.rs:2093)        fleet/action  (plugin.rs:2773)
              ▼                                      ▼
  answer.rs:66  ── ZERO acp handling ──▶     execute_acp_action (rpc/mod.rs:6083)
  capture_pane + tmux send-keys              pool.answer_permission ✅
  (answer.rs:218-245)
              ▼
  no tmux pane for an ACP task worktree -> Target::NoTarget -> row stays open
```

`rg acp crates/ainb-hangar-daemon/src/answer.rs` returns nothing. Control Center is the
surface an operator actually uses (`screen/control_center.rs:7`, `:539`) and it can only
answer via `attention/answer`.

**Fix, and it is the lazy one because it is the root-cause one:** an ACP arm at the TOP of
`answer::answer` (`answer.rs:66`), before target resolution. The row's payload already
carries everything (`acp_pool.rs:2525-2533`: `sessionKey`, `requestFingerprint`, `options`,
`rpcId`, `toolCall`):

```rust
if row.payload_kind() == "acp_permission" {
    // Routed by the row's OWN session_key -- no cwd correlation, so the C1
    // ambiguity guard has nothing to refuse.
    return acp_answer_result(
        acp_pool::active_handle().await,
        payload["sessionKey"], payload["requestFingerprint"],
        match_answer_to_option(&payload["options"], &params.answer),
    );
}
```

~30 lines. It also fixes ACP **chat** permissions from Control Center, broken today for the
same reason -- which is what makes it the root-cause fix rather than a task special case.
`mark_answered_if_open` stays the first-answer-wins gate; the actor's `retire_attention`
(`acp_pool.rs:2664`) calls the same conditional UPDATE, so a double call is benign.

### 3c. AskUserQuestion under ACP: unverified

**Probe before this is scoped as "AskUserQuestion answered from the TUI".** What the repo
does prove:

- `agent-client-protocol` pinned `=1.3.0` (`crates/ainb-acp/Cargo.toml:20`,
  `crates/ainb-hangar-daemon/Cargo.toml:77`, `Cargo.lock:57-60`).
- Exactly one agent-to-client REQUEST is wired: `session/request_permission`
  (`client.rs:746-760`). No options-carrying tool-call request exists in the v1 set this
  crate imports (`client.rs:52-58`).
- The reducer folds `ToolCall` and `ToolCallUpdate` into one opaque `ChunkKind::ToolCall`
  (`reducer.rs:312-314`). Nothing inspects a tool name, so an `AskUserQuestion` arriving as
  a plain tool call becomes an `acp.tool_call` row that **blocks nothing and asks nobody**.
- `rg AskUserQuestion crates/` hits only tmux/hook-era code, never anything ACP.

Three possible adapter behaviours, none settled here: (1) it surfaces as
`session/request_permission` with the question's options -- everything above already works,
and `options_wire` (`client.rs:258-270`) already carries `optionId/name/kind`; (2) the
adapter answers it itself and no human is ever asked; (3) the tool is not offered at all.

**Falsifier, 30 minutes:** `crates/ainb-acp/tests/real_adapter.rs` already exists as the
`#[ignore]`d real-adapter harness that CI explicitly skips (`.github/workflows/ci.yml:750-753`).
Add one probe that prompts a real `claude-agent-acp` to ask a two-option question and
records whether a `session/request_permission` fires. Until it runs, outcome (1) is a
**hypothesis**.

### 3d. What "interactive mode" becomes under ACP

- `Mode::Interactive` + `Executor::Acp` is **refused at dispatch**, not silently downgraded.
  `dispatch_mode` (`run_loop.rs:1155-1159`) terminalises with `ProvisionError` naming the
  flag. Silently running headless is the class of bug `interactive_command`
  (`run_loop.rs:1538`) was extracted to prevent.
- The interactive/headless axis is replaced by the permission mode: `default` (the adapter
  asks, the human answers through 3b) vs `bypassPermissions` (nothing asks, today's
  `--dangerously-skip-permissions` equivalent).
- **Constraint:** that mode is per adapter PROCESS (`config.rs:37`), so on a shared pool
  every session on `claude-agent-acp` shares one. Second independent reason for a per-task
  process (section 5).
- The tmux path is untouched and reachable only under `executor=process`.

---

## 4. Transcript unification

### 4a. Two durable stores, deliberately

| Executor | Durable transcript | Written at |
|---|---|---|
| process | `{logs}/<provider>.jsonl` | `runner.rs:1908` |
| acp | `fleet_provider_event`, `source='acp'`, `event_type='acp.<kind>'` | `crates/ainb-acp/src/store_writer.rs` |

Keep both. Dual-writing process stdout into SQLite would put thousands of rows per run
through the one write lock the whole control plane shares -- the exact hazard
`store_writer.rs:14-21` documents about ACP already being the new heavy writer.

### 4b. One read, server-side

`handle_board_card_timeline` (`rpc/mod.rs:11215-11277`) already resolves the card's newest
task and picks whichever of `claude.jsonl` / `codex.jsonl` exists (`:11259-11272`). Give it
one branch and one return shape:

```
task -> scope_key "task:<id>" resolves to a fleet_acp_session?
   yes -> FleetProviderEventRepo rows  -> transcript::classify_acp
   no  -> read_tail(logs/<p>.jsonl)    -> transcript::classify_stream_json
                    │
                    ▼
   BoardCardTimelineResult { task_id, provider, entries: Vec<TranscriptLine{kind, body}> }
```

`BoardCardTimelineResult.jsonl: String` (`snapshots.rs:2769-2772`) is replaced by
`entries`. The plugin's `apply_board_card_timeline` (`plugin.rs:1811-1840`) stops calling
`parse_timeline` and maps `entries` to `ViewEntry::line` -- ~5 lines changed, and the
classifier it used to call now runs daemon-side against both stores. Daemon and plugin ship
together (`ci.yml:667` stages the bundled plugins), so the wire change is internal.

**PR capture** keeps working unchanged: `run_acp` builds `RunnerResult.stdout_tail` from the
same classified entries, and `parse_gh_pr_create_stdout` (`run_loop.rs:1718`) finds the URL
in the agent's own message and in tool-result text.

### 4c. Live, per 2c

`TaskMessage` carries `(kind, body)` straight from the same classifier, so a live-appended
line and its later re-pulled twin are byte-identical. That is the payoff for moving the
classifier instead of writing a second one.

---

## 5. Sandbox and credentials

### 5a. What a task loses on a shared pool

| Control | Where | Reaches an ACP adapter? |
|---|---|---|
| OS sandbox (Seatbelt / Landlock) | `crates/ainb-hangar-sandbox/src/lib.rs:147`, wired `run_loop.rs:530-533` | No. The pool spawns a bare `tokio::process::Command` (`client.rs:682-706`). |
| env allowlist + `agent_env` | `compose_child_env` (`runner.rs`), built by `prepare_spawn_inputs` (`run_loop.rs:972-1042`) | No. Adapter env is per PROCESS. |
| claude credential ladder | `claude_cred.rs::resolve` via `resolve_cred_env` (`run_loop.rs:928-958`) | No, and probably unwanted: the adapter authenticates itself. |
| skills / profile materialisation | `run_loop.rs:1029-1041`, forwarded as `*_HOME` env keys | No (env-shaped). |
| per-task cwd | `location.cwd` (`run_loop.rs:1210-1225`) | **Yes** |
| per-task MCP | `agent.mcp_config` | **Yes** (`client.rs:436`) |

### 5b. Recommendation: per-task adapter process

Reasons, weightiest first:

1. **The permission mode is per process** (`config.rs:37`, asserted `client.rs:612`). A
   shared `claude-agent-acp` cannot host a `bypassPermissions` autopilot task and a
   `default` interactive task at once. That alone forecloses the shared pool for tasks.
2. **`agent_env` is per process.** It is the documented "ONE permitted plaintext escape"
   (`run_loop.rs:1417`); leaking one task's secrets to every other tenant is a regression on
   today's isolation.
3. **The sandbox is per process.** Confinement is what `ainb-hangar-sandbox` exists for
   (`lib.rs:1-10`).
4. **The ceilings are wrong**: `max_in_flight_per_process: 4` (`acp_pool.rs:261`) would cap
   the whole fleet's concurrent task turns at 4 per provider.
5. The per-provider `SlotCircuit` breaker (`acp_pool.rs:967-1000`) means one crash-looping
   task poisons every chat session on the same adapter.

**Cost, honestly:** one adapter process per running task instead of one per provider. That
is exactly today's model (one `claude` CLI per task), so not a regression, and the adapter
dies with the task (`kill_on_drop(true)`, `client.rs:698`).

**Mechanism.** `PoolConfig.adapters` is a `HashMap<String, AdapterConfig>`
(`acp_pool.rs:222`) and `provider_process` (`:1026`) keys the live process map by the same
string. Register a per-task adapter under `claude-agent-acp#task:<task_id>` whose config
carries: `command` = the sandbox launcher wrapping the real adapter (mirroring
`resolve_provider_path` `run_loop.rs:320` and `sandboxed_command` `lib.rs:147`);
`extra_env` = what `prepare_spawn_inputs` already returns (task_env + agent_env + skills
`*_HOME`); `permission_mode` per task. The pool then gives isolation for free: lazy spawn
on first prompt (`:1026-1128`), idle reap (`stop_idle_processes` `:914`), one process per
key. The only pool change is making `adapters` writable at runtime
(`register_adapter` / removal on teardown), ~40 lines.

**Landlock caveat:** `sandboxed_command` applies its ruleset in a `pre_exec` hook
(`lib.rs:24-30`) and returns a `std::process::Command` (`into_inner` `:131`);
`tokio::process::Command::from(std_cmd)` preserves it. On macOS the launcher is
`/usr/bin/sandbox-exec`, a plain program swap. Both fit `AdapterConfig { command, args }`
with no sandbox-crate change.

**Credential:** do NOT inject `CLAUDE_CODE_OAUTH_TOKEN` by default. The ladder exists
because a confined CLI child cannot reach the Keychain, and a static token goes stale in
~8h (`ainb-tui/CLAUDE.md`). Start with `env_passthrough: ["CLAUDE_CODE_OAUTH_TOKEN"]`
(already that field's documented purpose, `config.rs:38-40`) and prove which auth path the
adapter takes in the first live run before hard-wiring either.

---

## 6. Test plan

All of it runs with no adapter and no credentials, via
`crates/ainb-acp/src/bin/fake_acp_adapter.rs` (585 lines, 20 env knobs) and a fake provider
binary for the process side.

| # | Test | File | Asserts |
|---|---|---|---|
| T0 | classifier move is behaviour-preserving | `crates/ainb-hangar-proto/tests/` | the 6 moved `jsonl_timeline` tests pass verbatim against `transcript::classify_stream_json`. Move first, in its own PR, so a later regression is attributable. |
| T1 | **both executors stream live** | `crates/ainb-hangar-daemon/tests/tripwire_task_live_stream.rs` | the SAME assertion run twice, once per executor: subscribe to the workspace event stream, run a task, collect `TaskMessage` events, assert the `(kind, body)` sequence equals what the durable read (`board_card_timeline`) returns for the same run. **This is the test that pins the requirement.** A producer that emits nothing, or emits a different taxonomy than the re-pull, fails it. |
| T2 | ACP happy path | `tests/tripwire_acp_task_end_to_end.rs` | `HANGAR_TASK_EXECUTOR=acp` + fake adapter with `FAKE_ACP_ECHO_PROMPT=1`: `queued -> dispatched -> running -> done`; `result` carries the final message; `session_id` = the `session_key`; `fleet_provider_event` holds `acp.message` rows |
| T3 | permission answered from the attention path | same file | `FAKE_ACP_PERMISSION_SESSIONS=*`: an approval row appears with the worktree cwd; answering via **`attention/answer`** (not `fleet/action`) delivers; the turn completes; the row flips `answered`; `FAKE_ACP_RPC_LOG` shows exactly one `permission` line. Pins the 3b fix. |
| T4 | executor tripwire, both directions | `tests/tripwire_task_executor_flag.rs` | `=process` spawns a process (marker file) and writes `logs/claude.jsonl`; `=acp` spawns none, writes zero jsonl, writes N `fleet_provider_event` rows. Both asserted, because a flag that silently falls back keeps every other test green. |
| T5 | outcome mapping | `tests/acp_task_outcome.rs`, pure unit | table-drives section 2f including the unmapped-detail arm. No daemon, no adapter. |
| T6 | interactive + acp refused | in T4's file | terminalises `provision_error` naming the flag; `tmux has-session` on the exact name is non-zero |
| T7 | cancel | in T2's file | `FAKE_ACP_HANG_SESSIONS=*` then `hangar/issue_cancel_active`: task `cancelled`, `FAKE_ACP_RPC_LOG` carries `cancel:<adapter session id>` |
| T8 | unified durable read | `tests/timeline_both_executors.rs` | `board_card_timeline` returns the same `TranscriptLine` shape for a process run and an ACP run of the same brief |
| T9 | **argv golden matrix UNCHANGED** | `tests/argv_golden_matrix.rs` | run unmodified. Move 1 adds no argv. Do not regenerate: a diff means the `process` path moved and is a real break (`ainb-tui/CLAUDE.md`, Contract Tests). |
| T10 | env isolation | `crates/ainb-acp/tests/` (extends the allowlist tests) | `FAKE_ACP_ENV_DUMP` proves the per-task adapter got the task's `agent_env`, not the daemon's ambient secrets, and that task A never sees task B's |

**CI wiring.** `hangar-e2e` already runs every `tripwire_*` in `ainb-hangar-daemon/tests/`
via `scripts/hangar/run_all_tripwires.sh` (`ci.yml:703-725`), on ubuntu and macos, with the
daemon built at `:655-659`. T1/T2/T3/T4/T6/T7 named `tripwire_*` land there with **zero
workflow edits**. T5/T8 are plain `tests/` files needing the acceptance lane
(`ci.yml:730-734`) or an explicit `-p ainb-hangar-daemon` nextest line. T0 needs
`-p ainb-hangar-proto`, which is **outside `default-members`** -- a bare `cargo nextest run`
skips it and still reports clean (`ainb-tui/CLAUDE.md`). T10 rides the `acp` job
(`ci.yml:754-770`).

**Live probe, manual and separate:** 3c's `AskUserQuestion` probe as `#[ignore]` in
`real_adapter.rs`; the opt-in `chat-bus-smoke` lane (`ci.yml:817-840`) is where a real
end-to-end task run belongs once one exists.

---

## 7. Delivery slicing

Each step green on its own.

| # | Step | Size | Deletes / defers |
|---|---|---|---|
| 1 | **Classifier move.** `jsonl_timeline` -> `ainb-hangar-proto::transcript::classify_stream_json`, returning `Vec<(MessageKind, String)>`; 5-line `ViewEntry` wrapper stays in the plugin; the 6 tests move. T0. Pure refactor, no behaviour change. | **M** | deletes the plugin's copy |
| 2 | **Live stream for the PROCESS executor.** `RunnerConfig.sink`, per-line `TaskMessage` from `stream_stdout`, throttled `TaskProgress`, plus the task-detail append arm. Half of T1. **Ships value with no ACP at all**: the board timeline overlay and the run banner go live for the first time. | **M** | defers the ACP half |
| 3 | **ACP answer arm in `answer.rs`.** Section 3b. Standalone, and fixes ACP CHAT permissions from Control Center which are broken today. | **S** | nothing |
| 4 | **Pool prerequisites.** `stop=<StopReason>` on the DELIVERED detail (`acp_pool.rs:1936-1947`); extract `acp_session::ensure` and `acp_message::enqueue` out of the two RPC handlers; `register_adapter` on `PoolConfig`. No chat-path behaviour change. | **S** | the RPC handlers shrink |
| 5 | **`acp_task::run_acp` + the flag.** `DaemonConfig.task_executor`, `ResolvedDispatch.executor`, the `execute_claimed` arm, the cancel case, the leg poll, the outcome mapping, per-task adapter process. Live `TaskMessage` from the actor completes T1. T2, T4, T5, T6, T7, T9, T10. | **L** | defers usage/cost, PR capture (step 6), per-agent selection (step 8), resume across a task retry |
| 6 | **Unified durable read.** `board_card_timeline` ACP branch + `classify_acp`; `BoardCardTimelineResult.jsonl` -> `entries`; plugin maps to `ViewEntry`. PR capture from the same entries. T8. | **M** | defers `fleet/transcript_list` ever getting a TUI reader (it still has none, and does not need one) |
| 7 | **Usage.** Parse `acp.usage` rows into `ProviderUsage` so `persist_usage` (`run_loop.rs:1777`) and the usage dashboard see ACP runs. | **S** | |
| 8 | **Per-agent selection.** `agent.provider` accepting adapter tokens. No migration. | **S** | defers a per-task override column |

Steps 1-3 are useful on their own with the executor flag still absent, which is what makes
this orderable rather than one big-bang PR.

### Exit criterion for "Move 1 done"

Two halves, one per executor.

**Half A -- streaming, both executors, no ACP required:**
A card run under `executor=process` shows its transcript growing live in the board timeline
overlay while the run is in flight, and the same run under `executor=acp` shows the same
taxonomy from the same classifier. T1 is the mechanical form of this.

**Half B -- the Boxtrack P3 leg (`docs/hangar/proofs/fullstack/REPORT.md:26`) with zero tmux:**

1. Board card created, `Run` with `executor=acp`.
2. `tmux list-sessions` shows no `tmux_hangar-<task_id>` at any point; the run's `logs/`
   holds no `*.jsonl`.
3. The agent raises a real decision; an approval attention row appears with the worktree cwd.
4. Control Center (`C`) shows `1 need you` with the adapter's own option labels; the option
   key answers it through `attention/answer`.
5. The transcript shows the agent ACTING on the chosen option, not the default.
6. The task reaches `done`, the card advances, the timeline renders from
   `fleet_provider_event`.

Step 5 is the one that matters and the one defect 26 caught last time (`REPORT.md:62`: the
operator's pick was silently replaced by the highlighted default). ACP removes that bug
class because the answer is an option id on a JSON-RPC responder, not a keystroke into a
pane.

**Half B step 3 is gated on the 3c probe.** If `claude-agent-acp` does not surface
`AskUserQuestion` as `session/request_permission`, the criterion becomes a tool-permission
approval (a Write or a Bash the adapter asks about) -- which the fake adapter already proves
and which is still a full structured round trip with no tmux. Say so now rather than
discovering it at proof time.

---

## Contradictions with the brief, collected

1. **cwd IS per session** under the multiplex (`fleet_acp_session.cwd`, `acp_pool.rs:1528`,
   `:2340`). Only env, permission mode and the OS sandbox are per process.
2. **`permission_mode` is not "pinned at `session/new`"**: upstream v1 has no mode field, so
   the client asserts it post hoc and requires an echo (`client.rs:34-43`, `:612-665`).
   There is no `ask` mode; the axis is `default` vs `bypassPermissions`.
3. **`fleet/action` is not the path the TUI inbox uses.** Control Center answers via
   `attention/answer` (`plugin.rs:2093`) into `answer.rs`, which has zero ACP handling. An
   ACP permission is unanswerable from Control Center today, for chat as well as tasks.
4. **There is no live transcript channel to unify.** `HangarEvent::TaskMessage` and
   `TaskProgress` have consumers and no producers anywhere in `src/`; the board timeline's
   live append and the run banner are dead code. Move 1 writes the first producers rather
   than designing a new stream.
5. **The daemon cannot reuse the existing transcript parser** without moving it:
   `jsonl_timeline` lives in the plugin crate and returns a plugin type, and
   `ainb-hangar-core` cannot host it because `ainb-hangar-proto` depends on core, not the
   reverse (`crates/ainb-hangar-proto/Cargo.toml:22`).
6. **A "turn-complete future" does not exist and should not be added.** The delivery leg is
   the existing always-resolved signal.
7. **`agent-client-protocol` 1.3.0 exposes no options-carrying tool-call request.** The one
   agent-to-client request wired here is `session/request_permission`. Whether
   `AskUserQuestion` arrives that way is an adapter question this repo does not answer.
