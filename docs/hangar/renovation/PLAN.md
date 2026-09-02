# Hangar renovation plan: spine first, crisp in parallel

Status: proposed 2026-09-02, after the prove-hangar run (PR #815).
Decision record: https://explainers.stevengonsalvez.com/hangar-renovation/ (option C chosen).
Appendices in this directory: `exec-map.md` (how a task executes today, counted surface,
brittle seams), `move1-acp-tasks.md` (track A move 1 in full), `crisp-ui-track.md`
(track B in full). Every file:line in the appendices was read at `7d2b10d3`, which is
`main` after #815.

## The diagnosis in one paragraph

Hangar cloned Multica's surface (157 RPC methods, 20 screens, 123 CLI commands, 93
migrations) and missed its spine. Multica's spine is one noun (the issue), one execution
record per trigger (the task), one place to watch it (the issue page with a live run card).
Hangar has nine row-level concepts for "a thing an agent is doing", four state stores for
one unit of work, three boards, three attention surfaces, two disjoint write paths into one
database, and a human-in-the-loop that types digits into a screen-scraped tmux pane. Tasks
are NOT ACP: they run as provider CLI subprocesses (headless `claude -p` stream-json, or a
detached tmux session). ACP exists and works, for fleet chat only. Every screen holds the
data and paints the ULID.

## Two tracks

```
                 week 1            week 2            week 3+
track B  crisp   B1 names ─▶ B2 cards ─▶ B3 inbox ─▶ B4 detail ─▶ B5 tabs
(plugin only)    ▲ ships in days, each step recorded, no schema, no new RPC
                 │
track A  spine   A0 probe ─▶ A1 classifier ─▶ A2 process stream ─▶ A3 ACP answer arm
(daemon)                     ─▶ A4 pool prereqs ─▶ A5 run_acp + flag ─▶ A6 read/PR ─▶ A7 usage ─▶ A8 per-agent
                 then move 2 (issue-with-runs, one writer), move 3 (surface collapse), move 4 (`ainb hangar up`)
```

The tracks touch different files. Two seams connect them and are named so nobody
half-builds across them: B3 renders the same attention rows A3 makes answerable for ACP
(same `attention/answer` RPC, byte for byte); B4's transcript pane consumes A2's live
`TaskMessage` stream when it lands and backfills from the timeline read until then.

## Track A: the spine

### Move 1: two first-class executors, one live run-event stream (detail: `move1-acp-tasks.md`)

Operator requirements folded in: BOTH `claude -p` (stream-json) and ACP are first-class
task executors, BOTH stream results back to the task live, selection is
`HANGAR_TASK_EXECUTOR=acp|process` (default `process` until ACP is proven) with a per-agent
override later. Structured human-in-the-loop is ACP-only in move 1; the process path keeps
the hook route for AskUserQuestion.

| # | step | size | why it is orderable |
|---|------|------|---------------------|
| A0 | Probe: does `claude-agent-acp` surface `AskUserQuestion` as `session/request_permission`? One `#[ignore]` test in `crates/ainb-acp/tests/real_adapter.rs` | 30 min | gates the exit criterion; if no, the criterion becomes a tool-permission approval |
| A1 | Move the stream-json classifier from the plugin (`widgets/jsonl_timeline.rs`) into `ainb-hangar-proto::transcript`; pure refactor | M | one classifier for durable and live, both executors |
| A2 | Live stream for the PROCESS executor: `stream_stdout` emits `HangarEvent::TaskMessage` per line (the event exists in proto with no producer today), task detail appends live | M | value with zero ACP: the board timeline and run banner go live for the first time |
| A3 | ACP answer arm at the top of `answer.rs`: an `acp_permission` row answers through `pool.answer_permission`, not tmux | S | fixes ACP chat permissions from Control Center, broken today |
| A4 | Pool prerequisites: `stop=<StopReason>` on the delivered leg, extract `acp_session::ensure` / `acp_message::enqueue`, `register_adapter` on `PoolConfig` | S | no chat-path behaviour change |
| A5 | `acp_task::run_acp` + the flag: one new arm in `execute_claimed` (the shape `run_interactive` already has), per-task adapter process (sandbox, agent_env, permission mode are per process), leg poll bounded by `HANGAR_PROVIDER_MAX_RUNTIME_MS`, outcome mapping table, cancel arm | L | finalize path (success/failure/cancel, board advance, retry) untouched |
| A6 | Unified durable read: `board_card_timeline` reads `fleet_provider_event` rows for ACP runs; PR URL captured from the transcript | M | |
| A7 | Usage: `acp.usage` rows into `ProviderUsage` | S | |
| A8 | Per-agent selection via `agent.provider` = adapter token; no migration | S | |

Exit criterion, two halves: (A) a card run under each executor shows its transcript growing
live with the same taxonomy; (B) the Boxtrack P3 leg reproduced with zero tmux: no
`tmux_hangar-*` session, no `*.jsonl`, the ask lands as an attention row, the answer goes
through `attention/answer` to the adapter's pending JSON-RPC id, and the transcript shows
the agent acting on the chosen option (the class of bug defect 26 caught).

Decisions already taken in the appendix and adopted here: no `TaskExecutor` trait (a third
`if` arm, extract the trait when a fourth executor exists); no turn-complete future (poll
the always-resolved delivery leg); per-task adapter process, not the shared pool
(permission mode, `agent_env` and the sandbox are all per process, and the shared pool caps
task turns at 4 per provider); `interactive` + `acp` is refused at dispatch, not downgraded.

### Moves 2-4 (scoped after move 1 lands)

| move | what | size | deletes |
|------|------|------|---------|
| 2 | One noun, one writer: issue status becomes a CHECK'd category enum, runs attach to the issue, stages are sub-issues or stage columns on one board, `board_card` is position only; the CLI drops its 18 `Store::open_default` calls and speaks RPC through `ainb-hangar-client`; TUI applies deltas instead of refetching snapshots | L | Kanban task board, separate pipeline board, the nine best-effort twin writes in `board.rs`, `fleet_confirm` as a second answer mechanism |
| 3 | One surface: six screens (Issues, Issue, Inbox, Agents, Usage, Settings), chat as a pane; track B lands most of the visible half early | M | Control Center, Fleet queue, Boards, Kanban, Skills, Autopilots, Profiles, Squads, Logs, Daemon as tabs |
| 4 | `ainb hangar up`: start daemon, detect installed CLIs, register the host runtime with one default agent per CLI, open on an empty board; one config file replaces 30 env vars | M | install.json seeding, onboarding version checks, the Landlock-crashes-Bun surprise |

## Track B: crisp UI (detail: `crisp-ui-track.md`)

Plugin only. No schema, no new RPC (one trivially additive relaxation: `board_id` optional
on `board_card_timeline` so an uncarded issue can backfill its transcript). Every step is
recorded before/after with a vhs tape under `docs/hangar/proofs/crisp/`.

| # | step | size | the shot that proves it |
|---|------|------|-------------------------|
| B1 | Resolve every id to a name: agent names on task detail, usage, cards; inbox lines as `<agent> <verb> <HGR-n> <title>`; transcript backfill on open; R-on-done note; roster refresh on wizard open; `@` filter cursor; Ctrl+U in rename; branch elide; failed-first ordering (defects 5-9, 12, 21) | M | task detail says `Agent: impl-1`, inbox reads as sentences, detail opens with a transcript |
| B2 | Cards and hint bars: card footer swaps by state (`◔ impl-1 · running 2m · PR ✓`, `● impl-1 · ASK 40s`, `◇ blocked by MHAJBV`), `◇ None` never prints, hint grammar (5 contextual + 3 globals, verb first), Boards' second hint band deleted, `N working` chip wired, one `vocab.rs` for status words | M | idle, running and asking cards side by side; Boards with one hint bar |
| B3 | Inbox becomes the one attention surface: `needs you` block (attention rows, inline answer lifted from Control Center) above `recent` (inbox entries recomposed), filters; the board chip and Control tab become views of it | M | the P3 human loop driven entirely from `I` |
| B4 | Detail screen: sticky live run card (`◔ impl-1 is working · 7m 17s · 10 tools · $0.42`), one meta line, transcript + activity panes, facet panel gone; keeps the `Acceptance: 0/3` / `Props:` / `Meta:` literals four tripwires assert | M | run card ticking while the transcript streams |
| B5 | Tab strip 16 to 7 (`1 2 K B I A ,` plus `? q`), nine screens behind `^P Go:` and Settings, `Kanban` relabelled `Runs`, help rewritten; add `palette_reaches_every_demoted_screen` | S | 7 tabs at 80 columns, `^P usage` still lands |

Do not attempt in track B (they belong to the spine): collapsing issue/card/task state, any
plugin-side write that is not an existing RPC or any polling to hide defect 15, anything on
the answer delivery path, a live transcript for interactive runs (there is no capture),
unifying the attention / confirm / receipt stores, gating done on acceptance, board-card
authoring (defects 16, 17).

## Risks

| risk | sev | mitigation |
|------|-----|------------|
| `claude-agent-acp` does not surface AskUserQuestion as a permission request | high | A0 probe first; exit criterion falls back to a tool-permission approval, still zero tmux |
| Third-party adapters (Zed's claude-agent-acp, codex-acp) regress on the task path | med | pin adapter versions, per-provider crash breaker already exists, `process` stays default |
| Two big migrations in move 2 on a 93-migration schema | med | extend `migration_upgrade_full_chain`, never bypass it |
| Track B snapshot churn (13 card snapshots, 3 control-center snapshots) | low | one snapshot refresh per step, reviewed as images |
| Tripwires assert screen text | low | each B step names the tripwires it touches; four literals are kept verbatim |

## Open questions

1. AskUserQuestion under ACP (A0). Owner: first task of track A.
2. Adapter credential path: passthrough `CLAUDE_CODE_OAUTH_TOKEN` or let the adapter use the keychain? Prove on the first live smoke run, do not hard-wire.
3. Whether `agent.provider` should carry the executor (A8) or a new column; decided after A5 ships.
