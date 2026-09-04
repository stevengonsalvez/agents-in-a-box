# /goal The ainb TUI's sessions screen is the only place an agent asks for you and the only place you answer it, with the host Fleet panel, the host notifyd Inbox and ATC lite deleted, proven by tmux-driven verification and published as a recorded explainer.

— CONTEXT —
· Project: Collapse five competing "an agent needs you" surfaces in the ainb TUI down to one. The sessions screen carries per-row attention chips (ASK · APPROVE · ERR · DONE, with age) and answers them in place; its right pane becomes a tab strip (preview · ask · thread · copilot · log). The host Fleet panel — a second renderer of a state machine the hangar plugin already owns — is deleted, along with the host notifyd Inbox, the Codex-only `t` start form, and ATC lite mode. The fleet copilot becomes a general ainb assistant with a config-driven ACP engine registry and a help/guarded/yolo permission dial. LLM-free auto-continue of transient API errors moves into the hangar daemon as a sweep that needs no ATC instance. THE CONTRACT IS `docs/plans/attention-surface-spec.md` — read it first, in full; it is interview-locked and every decision in it is settled.
· Stack: Rust workspace at `ainb-tui/` (edition 2024, ratatui TUI in `ainb-core`); subprocess plugins over JSON-RPC (`ainb-plugin-hangar`); tokio daemon (`ainb-hangar-daemon`) with sqlx/SQLite (`ainb-hangar-store`); pure ACP client library (`ainb-acp`) over the upstream `agent-client-protocol` crate; shared lower half in `ainb-fleet-core`; tmux for session panes; insta snapshots; tmux-driven `tripwire_*.rs` end-to-end tests.
· Current state: Nothing built; the reviewed spec is the only artifact. Grounded 2026-09-04: `crates/ainb-core/src/components/fleet_panel.rs` is 2177 lines and imports `ainb_plugin_hangar::screen::fleet` at 9 sites while the hangar router binds `'F' => Screen::Fleet` at `screen/router.rs:122`, so the host panel is a second renderer of one reducer; `session_list.rs` paints `AlertKind::WaitingOnUser` at 1 site with 0 references to any answer path; `app/screens/builtin.rs` registers both `InboxScreen` and `FleetPanelScreen`; `crates/ainb-plugin-hangar/src/screen/inbox.rs` (1652 lines) already declares itself "the ONE attention surface"; `SupervisorMode::Lite` / `Controller::LiteScanner` appear 16 times in `fleet/atc/supervisor.rs`. Codex remote-control is already solved and on by default (`[codex] app_server` defaults to `desktop`, `ainb-hangar-daemon/src/lib.rs:496-499`), so the `t` key buys nothing and is simply deleted.
· Working dir: You are ALREADY in an isolated git worktree on your own branch — `ainb run` created it. Do not create another, do not switch branches, do not touch any other checkout. `pwd` is the worktree root; the Rust workspace is its `ainb-tui/` subdirectory. Your contract and this goal are committed at `docs/plans/attention-surface-spec.md` and `docs/plans/attention-surface-goal.md`.
· Constraints: EVERY acceptance check drives the REAL TUI through tmux (`/tmux-verify`, plus permanent unseeded `tripwire_*.rs`) — unit and insta tests are necessary, never sufficient, and a green suite is not evidence the feature works. EVERY phase publishes an `/explain-to-me` page with a full recording; a recording of a buggy run is DISCARDED and never published — record only once the run is fully clean. Loop until zero defects and observable behaviour matches the spec. TUI changes are additive: nothing working may break unless its replacement lands in the same change. Commits atomic, single-concern, conventional, signed (`-S`), no AI/Claude attribution in messages. `gh pr merge --merge` only — never `--squash`, never `--admin`. `.agents/` is gitignored, so anything that must persist goes to a tracked path (`docs/plans/` — note `/plans/` itself is gitignored here). Do NOT touch the hangar plugin's `F`/`I`/`C` tabs, ATC full mode, the ACP wire protocol, or any spawn path. NEVER `tmux kill-server`, `pkill tmux`, or any bulk/wildcard tmux kill — kill only by exact session name (`tmux kill-session -t "=name"`, quoted, because zsh eats a bare `=`).
· Audience: Stevie, and anyone running many concurrent coding-agent sessions in the ainb TUI. Internal tool, held to a shipped-product bar.

— SUCCESS CRITERIA (ALL MUST BE TRUE) —
1. A tmux-driven run proves the full loop on the real binary with the hangar daemon STOPPED and again with it RUNNING: an ASK appears as a chip on its session row, the `ask` tab answers it, the chip clears, and the agent proceeds — with `f`, `b` and `t` gone, and `rg` reporting zero references to `FleetPanelScreen`, `InboxScreen`, `SupervisorMode::Lite` or `Controller::LiteScanner`.
2. `cargo test --workspace` is green, including NEW permanent unseeded tmux tripwires covering: answer-with-daemon-down, answer-with-daemon-up, copilot engine swap on one channel, the help/guarded/yolo dial, and the daemon retry sweep continuing then escalating a seeded transient ERR with zero ATC instances provisioned.
3. One `/explain-to-me` page is published carrying a full recording of a CLEAN run only, every diagram matching shipped behaviour, and a PR is open whose required checks are all terminal and green.
4. Final deliverable runs without errors
5. You can show proof (screenshot · test output · URL)

— OPERATING RULES — NON-NEGOTIABLE —
1. PLAN FIRST. Output a numbered task list before writing any code.
2. WORK AUTONOMOUSLY. Don't ask clarifying Qs unless genuinely blocked.
3. SELF-VERIFY. After every step: run tests, inspect output, confirm it worked.
4. DEBUG YOURSELF. If it fails, diagnose + fix. Don't hand it back.
5. USE EVERY TOOL. MCPs · terminal · web · code exec · pull real data.
6. NO PLACEHOLDERS. No TODOs · no stubs · real components + real states.
7. PROGRESS LOG. Track completed · in-flight · decisions · blockers.
8. STAY ON GOAL. Discoveries off-spec? Note + keep moving.
9. IF BLOCKED. Log the wall · continue everything parallelizable.
10. CHECK SUCCESS BEFORE STOPPING. Re-read criteria · confirm each is met.

— QUALITY BAR —
· Code: clean, typed, follows project conventions
· Design: looks like a well-funded startup shipped it
· Output: survives a senior code review
· Docs: every new pattern / env var / decision logged

— FINAL DELIVERABLE —
✅ Confirmation each criterion is satisfied
📂 Every file created / modified
🚀 How to run / test / deploy
📊 Proof (screenshot · test output · URL)
📝 Decisions made + anything to know
⚠️ Known limitations + follow-ups

Begin by outputting your plan. Then execute end-to-end without checking
in until done or genuinely blocked.

---

# RUN PROTOCOL — read before planning

## Autonomy envelope (locked by Stevie, 2026-09-04)

- RESOLVE all 8 of the spec's open questions yourself. Record each decision and
  its rationale in the progress log. Do not stop to ask.
- STOP at a green PR. Merging is Stevie's. Never `--admin`.

## The verify-then-record loop — this is the whole method

```
┌──────────┐   ┌──────────────┐   ┌──────────┐   ┌───────────────┐
│  build   │──▶│ /tmux-verify │──▶│  clean?  │──▶│ record + page │
│  phase N │   │ REAL binary  │   │          │no │  /explain-to-me│
└──────────┘   └──────────────┘   └────┬─────┘   └───────────────┘
      ▲                                │ no             ▲ yes
      │        ┌───────────────────────┘                │
      │        ▼                                        │
      │   ┌──────────┐   discard the recording,          │
      └───│ fix bug  │   it is never published ──────────┘
          └──────────┘
```

Rules the loop exists to enforce:

- A buggy run is NOT recorded and NOT published. Fix first, then record once.
- Never publish a page showing a failure, a workaround, or a "known issue"
  you could have fixed. The page shows the shipped behaviour, working.
- A recording that needed a retake because the feature was broken is deleted,
  not kept as an appendix.
- "Tests pass" is not the gate. The gate is: the real binary, driven through
  tmux, did the thing a person would do, and the screen showed the right result.

## Per-phase definition of done

A phase is done only when ALL of these hold. No partial credit.

| gate | evidence |
|---|---|
| behaviour matches the spec | quote the spec line, show the tmux capture that satisfies it |
| driven on the real binary | `/tmux-verify` transcript, not a unit test |
| daemon-down path proven | same journey with the hangar daemon stopped |
| permanent test left behind | a new unseeded `tripwire_*.rs`, passing |
| whole suite still green | `cargo test --workspace` |
| nothing regressed | the surface it replaced is gone AND its replacement works |
| recorded clean | one take, no bugs visible |
| published | `/explain-to-me` page updated with that recording |

## Phase order (dependencies are real; do not reorder)

1. **Chips.** `session_list.rs` paints ASK · APPROVE · ERR · DONE with age;
   header counts ASK+APPROVE only. Replaces `[?]` `[!]` `[✓]`. No answering yet.
2. **Attention merge.** One `AttentionRow` view model fed local-first (hooks +
   notifyd), enriched by hangar `attention/list` when the daemon is up, with an
   explicit precedence rule and an `answerable` flag carrying its reason.
3. **Tab strip.** Right pane gains preview · ask · thread · copilot · log.
   Disabled tabs dim, never hide. `Enter` becomes tab-scoped
   (preview→attach, ask→send, thread/copilot→send message, log→no-op);
   attach digits keep working from every tab.
4. **Answering.** The `ask` tab sends: ACP rows through the daemon as a real
   prompt, tmux rows through `ainb-fleet-core::send`. In-flight, failure and
   revert-to-ASK all visible. THIS is the phase that must pass daemon-down.
5. **Delete the Fleet panel.** Screen, `f` binding, registration, host renderer.
   The canonical reducer stays in `ainb-plugin-hangar` untouched.
6. **Delete the host Inbox.** `b` binding and screen go; the `log` tab carries
   per-session history.
7. **Copilot.** Adapter registry from daemon config; provider becomes a
   registry-validated string; swap retires the old ACP session and mints a new
   one on the SAME channel; engine/model/mode header. The mode dial moves the
   daemon-side copilot guardrail ONLY — adapter `permission_mode` stays pinned
   at `session/new` and `copilot_configure` keeps rejecting it, because
   loosening that per session is the documented ambient-bypass bug.
8. **Broadcast + rehoming.** Broadcast acts on the sessions list's existing
   multi-select checkboxes; channels list inside the `copilot` tab; new-ATC
   moves to the Daemons screen. Delete the `t` start form.
9. **ATC lite deletion + daemon retry sweep.** Delete `SupervisorMode::Lite`,
   `Controller::LiteScanner`, `lite_heartbeat_id`, `ainb fleet atc supervise`,
   `--set lite`, and the `heartbeat-state.json` `continue_counts` ledger. Lift
   lite's pure `plan()` into a daemon-wide sweep that needs no ATC instance,
   backed by the durable `atc_retry` ledger, escalating at cap through
   `raise_escalation` into the attention pipeline.
10. **ACP chat repair.** Fix the two symptoms Stevie actually hit: the copilot
    pane never reaching a live composer from a cold install, and a send whose
    leg stays PENDING with no visible state. Both must show what they are doing
    and which call failed — never a blank box.

## Known traps — you will hit these

- A path-carrying text channel splits on the data. Keep paths in arrays; let
  only integers cross a text boundary. This bit three times in one shell script
  in this repo, once deleting source.
- `tmux has-session -t name` matches by PREFIX; liveness needs `-t =name`, and
  in zsh the `=name` must be quoted or the shell eats it.
- Renumbering or rebinding a hotkey requires a cross-crate tripwire sweep;
  daemon-crate tests assert on tab indices.
- Adding an enum variant or a manifest field breaks every exhaustive matcher and
  every struct literal — a MERGEABLE-CLEAN merge is not a compiling merge.
- macOS AMFI SIGKILLs a staged binary (exit 137, no stderr); the first-run
  wizard intercepts keystrokes; an EnvFilter crate-name drift silently hides
  logs; substring-OR assertions pass while the feature is broken. The
  `tmux-ui-tripwire` skill documents all four — read it before writing a
  tripwire.
- A required check that reports SKIPPED (path filters) is not a blocked PR.
  `BLOCKED` usually just means something has not reached a terminal state yet.

## Progress log

Maintain a running log with: completed phases, phase in flight, every one of the
8 open-question decisions with its rationale, blockers, and the URL of each
published page. Commit it to a tracked path (`docs/plans/`) — `.agents/` is gitignored.
