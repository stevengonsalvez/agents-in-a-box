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
| P1 single-issue happy path | PASS (proof run HGR-2) | TUI wizard create with repo `@boxtrack` + agent impl-1; daemon claim; worktree `ainb/01M1FKF4BD...` on the real repo; agent committed 3 conventional commits, pushed, opened boxtrack-proving PR #1; task `done` exit 0, `task.branch` recorded, run_history success (84 in / 8946 out), issue auto-promoted `done`; Kanban done card, Usage totals $1.66 | pending (clean re-drive) |
| P2 pipeline + squad | pending | - | - |
| P3 live human loop | pending | - | - |
| P4 levers + observability | partial | `R` retry override proven (child attempt chained, ran); Kanban, Usage, Daemon health rendered live | - |
| Docs refresh | pending | R-retry doc/code contradiction resolved empirically: operator `R` force-requeues failed/cancelled, silently no-ops on done | - |

Discovery run HGR-1 (before the env fix): attempt 1 failed `agent_error` in 16ms (defect 4), attempt 2 via `R` completed check+test green but the agent refused to commit because it had inherited the operator's global CLAUDE.md signing mandate and found no GPG key; hangar still finalized the task `done` and promoted the issue with 0/3 acceptance criteria ticked (design note: criteria do not gate done).

## Defects on the driven path

| # | defect | class | state |
|---|--------|-------|-------|
| 1 | task-detail lifecycle only advanced by live events; a task finalized before the screen subscribed left `R`/`X` dead | seam: snapshot vs event mapping | FIXED 51f54a9f 8ba928f5 |
| 2 | issue-list-opened detail bound a synthetic `task-<issue>` id; retry/cancel could never hit the real row | seam: synthetic vs real id | FIXED a35334de b1b9f7f7 |
| 3 | host globals (`H`, `W`) steal printable keys from plugin text inputs (help overlay opened mid-typing, chars dropped) | input routing | queued |
| 4 | Linux Landlock sandbox (default ON) aborts Bun-based claude at spawn; every headless dispatch fails `agent_error` | provider runner | queued |
| 5 | phantom "a run is already active (queued)" dispatch-refusal note with only terminal task rows; text duplicated | guard/note | queued |
| 6 | repo roster fetched once at plugin connect; repos added later unpickable until restart | snapshot staleness | queued |
| 7 | after `R`, detail stays bound to the old attempt; child transcript invisible until reopen; no transcript backfill on open | rebind | queued |
| 8 | agent name unresolved: task detail `Assignee: agent:<ulid>` / `Agent: -`; Usage per-agent rollup shows raw ULID | display mapping | queued |
| 9 | `R` on a done task silently no-ops (store returns DoNotRetry, no note) | feedback | queued |
| 10 | wizard render lags seconds behind typed input (state intact); `set_issues` replaces `IssueListState` wholesale and would destroy an open wizard when a snapshot lands | render starvation / state clobber | under investigation |
| 11 | `pr_url` not captured from a real `gh pr create` under stream-json; PR badge absent though PR #1 exists | capture | queued |
| 12 | wizard `@` filter narrows the list but the cursor stays on `scratch`; Enter picks scratch | picker UX | queued |

Docs drift found: wizard rows Accept / Context / Priority / Due / Labels missing from `tui-keybindings.md`.
