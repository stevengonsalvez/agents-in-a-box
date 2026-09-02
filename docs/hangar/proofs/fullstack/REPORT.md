# prove-fullstack: live proving report

Run started 2026-09-01 on branch `f/prove-hangar` (PR #815). Contract: `docs/hangar/prove-fullstack-goal.md`.
Every leg below was driven through the TUI on an isolated second ainb instance and cross-checked
against sqlite + git ground truth. Recordings are of green runs only.

## Environment disclosure

| item | value |
|------|-------|
| provider | REAL `claude` CLI 2.1.257, model sonnet, headless `-p --output-format stream-json` |
| agents | impl-1 (created via TUI `A` wizard); rev-1 / qa-1 pending P2 |
| app repo | https://github.com/stevengonsalvez/boxtrack-proving (private, throwaway), cloned under the iso home |
| isolation | fake `HOME=/home/claude/ainb-e2e-home`, private tmux server (`TMUX_TMPDIR`), worktree-built `ainb` + daemon, `AINB_PLUGIN_ROOT` pinned |
| agent auth | credentials-only `~/.claude` in the iso home (symlinked `.credentials.json`, copied `.claude.json`); no operator CLAUDE.md, hooks, or skills reach the agents |
| sandbox | `HANGAR_DAEMON_DISABLE_SANDBOX=1` on instance #2 after defect 4 (Landlock crashes Bun-based claude at spawn). Security downgrade, disclosed. |
| repo roster | `cache/repositories.json` seeded with the boxtrack path (what New Session's scan would have written) |

## Leg table

| leg | state | evidence | recording |
|-----|-------|----------|-----------|
| Phase 0 harness | PASS | daemon on iso socket (pid file, token, db under iso home); TUI `g` chrome; boxtrack repo seeded on main | - |
| P1 single-issue happy path | PASS (proof run HGR-2, recorded run HGR-3) | HGR-2: TUI wizard create with repo `@boxtrack` + agent impl-1; daemon claim; worktree `ainb/01M1FKF4BD...` on the real repo; agent committed 3 conventional commits, pushed, opened boxtrack-proving PR #1 (merged); task `done` exit 0, `task.branch` recorded, run_history success, issue auto-promoted `done`. HGR-3 (recorded): same journey driven entirely by the tape, agent added `GET /api/version` + test in 2m09s, pushed, opened PR #6; Kanban done card renders `impl-1 · done · ainb/01M1FKWT...` | `p1-happy-path.gif` / `.mp4`, stills `p1-1..6` (`p1-2` shows the render lag of defect 10: Repo/Agent still read as placeholders while the DB already held boxtrack + impl-1) |
| P2 pipeline + squad | PASS (HGR-5) | agents rev-1/qa-1 created via TUI `A` wizard; squad `shippers` + roles via TUI `S` (`c`, `a`, `r`); `pipeline init` + stage prompts via CLI (TUI gap); fan-out via TUI `S` then `x`. Card walked Triage → Implement → Review → QA → Done on its own: 4 tasks chained by `parent_task_id`, every one pulled through the role gate (`board_column_id` set), triage+implement by impl-1 (35s, 175s), review by rev-1 (40s, distinct agent: excludes_prior_agent), QA by qa-1 (86s) which posted the evidence comment and merged boxtrack PR #7; tickets CRUD now on boxtrack main | pending (clean re-drive) |
| P3 live human loop | pending | - | - |
| P4 levers + observability | partial | `R` retry override proven (child attempt chained, ran); Kanban, Usage, Daemon health, Boards (stage columns, wip caps, roles-covered banner), Squads live roster rendered during real runs | - |
| Docs refresh | pending | R-retry doc/code contradiction resolved empirically: operator `R` force-requeues failed/cancelled, silently no-ops on done | - |

Discovery run HGR-1 (before the env fix): attempt 1 failed `agent_error` in 16ms (defect 4), attempt 2 via `R` completed check+test green but the agent refused to commit because it had inherited the operator's global CLAUDE.md signing mandate and found no GPG key; hangar still finalized the task `done` and promoted the issue with 0/3 acceptance criteria ticked (design note: criteria do not gate done).

## Defects on the driven path

| # | defect | class | state |
|---|--------|-------|-------|
| 1 | task-detail lifecycle only advanced by live events; a task finalized before the screen subscribed left `R`/`X` dead | seam: snapshot vs event mapping | FIXED 51f54a9f 8ba928f5 |
| 2 | issue-list-opened detail bound a synthetic `task-<issue>` id; retry/cancel could never hit the real row | seam: synthetic vs real id | FIXED a35334de b1b9f7f7 |
| 3 | host reserved `?`/`H` off the plugin's per-frame `captures_text` flag, which lags one render; a typed `H` opened host help and the help-visible branch then swallowed EVERY key until Esc (read as "the wizard lost my input") | input routing | FIXED 74baddf7 728c8f7a |
| 4 | Linux Landlock sandbox (default ON) aborts Bun-based claude at spawn; every headless dispatch fails `agent_error` | provider runner | queued |
| 5 | phantom "a run is already active (queued)" dispatch-refusal note with only terminal task rows; text duplicated | guard/note | queued |
| 6 | repo roster fetched once at plugin connect; repos added later unpickable until restart | snapshot staleness | queued |
| 7 | after `R`, detail stays bound to the old attempt; child transcript invisible until reopen; no transcript backfill on open | rebind | queued |
| 8 | agent name unresolved: task detail `Assignee: agent:<ulid>` / `Agent: -`; Usage per-agent rollup shows raw ULID | display mapping | queued |
| 9 | `R` on a done task silently no-ops (store returns DoNotRetry, no note) | feedback | queued |
| 10 | `set_issues` rebuilt `IssueListState` wholesale on every issues snapshot (armed by any daemon push), destroying an open wizard, filters and selection mid-typing | state clobber | FIXED d9b985cb 55a290df |
| 11 | `pr_url` not captured from a real `gh pr create` under stream-json; PR badge absent though PR #1 exists | capture | queued |
| 12 | wizard `@` filter narrows the list but the cursor stays on `scratch`; Enter picks scratch | picker UX | queued |
| 13 | daemon logs ERROR `Codex managed transport degraded` every ~16s forever when codex is simply not configured | log noise | queued (run uses `AINB_CODEX_MANAGED=0`) |
| 14 | plugin showed "Hangar daemon offline" with the daemon alive and bound (14x `plugin/render exceeded its budget (2s)` burst first); never re-dialed | reconnect | queued |
| 15 | CLI verbs write the store directly with no event, so a running TUI never learns of a CLI-created board/issue until restart | CLI/TUI seam | queued |
| 16 | Boards card create/edit has no brief stage; a card-minted issue has an empty description | authoring gap | queued |
| 17 | no TUI path creates a briefed issue WITHOUT push-dispatching it, so pipeline work cannot be authored end to end in the TUI | authoring gap | queued |
| 18 | `board_card_create` never emitted `IssueCreated`; the minted issue was invisible on the issue list until restart | missing event | FIXED 364934c5 41f2cc8e |
| 19 | issue promoted to `done` when the card advanced INTO its last gated stage (only columns strictly right were counted), and `PULL_SQL` excludes done issues, so QA could never be pulled | pipeline lifecycle | FIXED 7a50a446 2d592f2b db2db561 |
| 20 | reviewer stage under one GitHub identity cannot `gh pr review --approve` its own PR; rev-1 stopped to ask a human | stage prompt | worked around: Review/QA prompts now use verdict comments |

Docs drift found: wizard rows Accept / Context / Priority / Due / Labels missing from `tui-keybindings.md`.

## Run notes

- Discovery run HGR-5 stalled at QA for 26 min (defect 19); repaired with `issue update --state in_progress` after the fix, QA pulled within 5s.
- rev-1's Review ran before the verdict-comment prompt landed; qa-1 merged on its own green QA. Disclosed: no reviewer approval artefact exists on PR #7.
- Session paused ~8h overnight on an expired Claude login (cron heartbeats queued, resumed 08:36 after `/login`).
