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
| P2 pipeline + squad | PASS (HGR-5) | agents rev-1/qa-1 created via TUI `A` wizard; squad `shippers` + roles via TUI `S` (`c`, `a`, `r`); `pipeline init` + stage prompts via CLI (TUI gap); fan-out via TUI `S` then `x`. Card walked Triage → Implement → Review → QA → Done on its own: 4 tasks chained by `parent_task_id`, every one pulled through the role gate (`board_column_id` set), triage+implement by impl-1 (35s, 175s), review by rev-1 (40s, distinct agent: excludes_prior_agent), QA by qa-1 (86s) which posted the evidence comment and merged boxtrack PR #7; tickets CRUD now on boxtrack main. Recorded re-drive (HGR-6 `Ticket stats`): tape-driven fan-out, 4 stages in 3m26s (18s/89s/35s/64s), `REVIEW: APPROVED` and `QA: PASSED` verdict comments, PR #8 merged by qa-1 | `p2-pipeline.gif` / `.mp4`, stills `p2-1..9` |
| P3 live human loop | PASS (proof run, card `#MNT1RV`) | Boards `b` new board, `n`/`r` column, `c` card (title as prompt, repo `@boxtrack`), `Enter` → `Run ▾` → Interactive; real claude in `tmux_hangar-<task>`; agent raised a real `AskUserQuestion` (2 options); Control Center `C` showed `1 need you` + ①②; pressed `2`; store row `answered\|tui\|api/app.db`; board `0 need you`; agent pane `→ api/app.db`; session JSONL `tool_result` = `"api/app.db"`; agent applied it (`DEFAULT_DB_FILE = 'api/app.db'`). Took FIVE fixes to get there (defects 22, 24, 25, 26, 14). Recorded clean take: session `cc2514da`, tape answered `2`, transcript line `="api/app.db"` printed in the same terminal after quitting the TUI | `p3-human-loop.gif` / `.mp4`, stills `p3-1..8` |
| P4 levers + observability | PASS | Levers proven live: `R` retry override (child attempt chained, ran), `X` cancel of an interactive run (row cancelled, tmux session reaped), Boards `b`/`n`/`r`/`c`/`e`/`d`, `w` depends-on then run refused `blocked by unfinished cards (MHAJBV)` with zero tasks created, Settings notification grid `space` (DB `ask_user_question` channels `phone,web,os,atc` → `web,os,atc` → restored), Squads roles, Agents wizard. Observables walked after real runs: `?` help (now complete), Kanban, Usage ($ per agent, run list), Daemon health, Logs with `e` filter, Inbox, Fleet `5` lens. Not exercised: workspace create/switch, skills attach, profile tier (no profiles), Ctrl+P palette | `p4-levers.gif` / `.mp4`, stills `p4-1..12` |
| Docs refresh | PASS | `tui-keybindings.md` refreshed (4dbd643f): Fleet/Agents/Boards/Control/Squads/Kanban/Logs/Inbox/Profiles sections, palette, wizard rows, `R` documented as the operator override; Help overlay rewritten as data with a router-coverage test (01219eb2) | `p4-1-help.png` |

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
| 14 | plugin showed "Hangar daemon offline" with the daemon alive: the daemon idle-closed any connection with no REQUEST for 600s (built for request/response clients), but the TUI holds one long-lived subscribed push channel and sends nothing while the operator watches a run; the plugin read EOF and never re-dialed | reconnect | FIXED e3598584 (subscribed connections exempt from idle-close) 9f3a45d3 (plugin auto re-dial with backoff) |
| 15 | CLI verbs write the store directly with no event, so a running TUI never learns of a CLI-created board/issue until restart | CLI/TUI seam | queued |
| 16 | Boards card create/edit has no brief stage; a card-minted issue has an empty description | authoring gap | queued |
| 17 | no TUI path creates a briefed issue WITHOUT push-dispatching it, so pipeline work cannot be authored end to end in the TUI | authoring gap | queued |
| 18 | `board_card_create` never emitted `IssueCreated`; the minted issue was invisible on the issue list until restart | missing event | FIXED 364934c5 41f2cc8e |
| 19 | issue promoted to `done` when the card advanced INTO its last gated stage (only columns strictly right were counted), and `PULL_SQL` excludes done issues, so QA could never be pulled | pipeline lifecycle | FIXED 7a50a446 2d592f2b db2db561 |
| 20 | reviewer stage under one GitHub identity cannot `gh pr review --approve` its own PR; rev-1 stopped to ask a human | stage prompt | worked around: Review/QA prompts now use verdict comments |
| 21 | Boards rename input: Ctrl+U typed as `u`; no clear/select-all | input | queued |
| 22 | interactive claude launched in a fresh worktree parked forever at the trust dialog, then at the bypass-permissions acceptance (nobody at the pane); run dies on the deadline | interactive launch | FIXED 0ef610a6 9b3b5491 014adfa9 (`pre_trust_claude_workdir`: projects[] trust + `skipDangerousModePermissionPrompt`) |
| 23 | daemon stop while an interactive task runs: task finalized `failed/spawn_error` (recovery respawn collides with the surviving tmux session) and the session is NOT reaped | recovery | queued |
| 24 | `attention/answer` reply swallowed by the TUI: no_target / ambiguous / delivery_failed / already_answered all rendered as silence while the agent stayed blocked | feedback | FIXED eeb221b9 e40b2eb3 |
| 25 | C1 target resolution compared the hook's cwd to the session root for string equality; after the agent `cd`s into `api/` every answer is refused `no_target` | answer routing | FIXED 4178f216 |
| 27 | third ASK from one session went unseen because the plugin was already in the FIND-14 offline state; the row stayed open, the board read `0 need you` | reconnect fallout | covered by 14 |
| 26 | **operator's pick silently replaced by the default**: answers were typed as text + Enter into the AskUserQuestion picker, which ignores text, so Enter accepted option ① while the store recorded option ② | answer delivery | FIXED d195fa86 50c5ba9e (route by position, verify picker closed, never type an option) |

Docs drift found: wizard rows Accept / Context / Priority / Due / Labels missing from `tui-keybindings.md`.

## Run notes

- Discovery run HGR-5 stalled at QA for 26 min (defect 19); repaired with `issue update --state in_progress` after the fix, QA pulled within 5s.
- rev-1's Review ran before the verdict-comment prompt landed; qa-1 merged on its own green QA. Disclosed: no reviewer approval artefact exists on PR #7.
- Session paused ~8h overnight on an expired Claude login (cron heartbeats queued, resumed 08:36 after `/login`).
- P3 first attempt: store said `answered|tui|api/app.db` and the board flipped to `0 need you`, but the agent pane read `→ data/boxtrack.db (Recommended)`. The converged harness's CC01/CC18 legs deliver into a plain-shell tmux target, so this mismatch was invisible to every existing test; only a live agent picker exposes it.
- Ship-phase live probe (2026-09-02 14:53 UTC, iso tmux `picker-probe-*`, killed by exact name after): Claude Code 2.1.258 commits an `AskUserQuestion` pick on the number key alone; the P3 recorded take (2.1.257, Down+Enter delivery) predates that finding, so the answer path now handles both.
- Lifecycle hooks for the agents were installed into the ISO home only (`ainb fleet atc setup p3hooks --no-heartbeat --no-spawn` under the iso env): `$ISO/.claude/settings.json`, hook script + `ainb-bin` pointer under `$ISO/.agents-in-a-box/hooks/`. No host-global unit or settings file was touched (verified by mtime).

## Ship phase: review

Heavy review on the whole branch diff at `a6dcba43` (five Opus personas: correctness, project standards, security, tests, api-contract, plus a Codex peer at high effort, read-only). Every finding fixed, P3 nits included, one signed commit per file (`cd345103..5a03f8c0`, 35 commits). Themes:

| area | round-1 finding | fix |
|------|-----------------|-----|
| `interactive.rs` pre-trust | fixed tmp name raced across launches; every read error fell through to an empty stub; mode widened to 0644; host-global `bypassPermissionsModeAccepted` written unconditionally | unique tmp + process lock, `NotFound` only, mode copied, global key dropped; bypass acceptance now rides the interactive argv as `--settings {"skipDangerousModePermissionPrompt":true}` (golden regenerated) |
| `answer.rs` routing | ancestor-cwd match too wide; text fallback typed into a picker; no verification the picker closed on the chosen option | nested matches gated on a raise transcript; digit + Enter with highlight and echo checks; text only when no picker is visible |
| `rpc/mod.rs` idle bound | subscribed connections exempt from any idle bound (leak on a wedged peer); `board_card_create` bypassed the issue-create seam (no activity row, no assignee, no `IssueCreated`) | 24h subscribed bound with env override, forwarders counted only while alive; card-minted issues go through `snapshots::issue_create` |
| `pull.rs` stage lifecycle | push-path runs left `board_column_id` NULL so a card parked in a gated column never finished; `STAGES_REMAIN_SQL` joined boards of other workspaces | `current_gated_column` stamps the run; join scoped by workspace; generation MAX over stage tasks only; index on `board_card(issue_id)` |
| plugin | host globals suppressed by the per-frame capture flag (Esc/Ctrl+C died on an unavailable plugin screen); refusal note outlived its card; `replace_rows` reset selection and confirmed deletes against the wrong row; non-exhaustive status and verdict mapping; help overlay clipped from the top on short panes; dropped-link redraw unbounded | help ownership scoped to `PLUGINS_WITH_OWN_HELP`; note keyed to the attention id; selection followed by id, stale confirms dropped; `TaskStatus::ALL` + exhaustive `answer_verdict_note`; clip at the viewport edge; `RECONNECT_REDRAW_WINDOW` |

Round 2 (single Opus verify pass over the fix delta) dispositioned all 58 round-1 items: 44 FIXED, 5 REBUTTED with evidence, 9 OPEN (mostly P3), plus 16 new findings in the delta. All fixed or rebutted in `51e5a35b..17a2d8f5` (13 per-file commits). The one that mattered: the round-1 fix had made picker answers press the option digit, wait for the highlight to move, then Enter. Probed live against Claude Code 2.1.258 (iso tmux, real `AskUserQuestion`): the digit alone COMMITS the pick (`· Which color? → Green` within 1.5s, no Enter), so every picker delivery would have timed out as "highlight did not move" and reopened an already-answered row. `picker_step` now reads the pane for either behaviour. Other round-2 fixes: pre-auth first-frame bound + connection cap on the RPC listener; card create threads the assignee through `issue_create` and emits the create's own row; per-board stage generations for an issue carded on two boards; the dead `hasTrustDialogHooksAccepted` key dropped (0 occurrences in the 2.1.258 bundle); the verdict note filed against the card that was answered; `stages_remain` shape test restored. Rebutted with evidence: `serde_json` `preserve_order` is already on in the daemon graph (test pins key order), card-title prefixing matches how every issue briefs its run.

Round 3 (verify pass over the round-2 delta): zero open at P2 and above; six P3s plus two carried residuals, all fixed in `c9709a33..fe4d3306`: the closed-picker settle window (the echo renders ~1.5s after the close, so the wrong-option guard now actually runs), free text refused when any picker chrome is on screen (Enter into a permission dialog is a tool approval), the question text matched so a look-alike later picker is not taken for ours, the stage stamp scoped to the launching board, one wire id per attention answer, the connection-cap log latched, one help row per section. Locally the tmux tripwires need a private tmux server (a shared server sizes a detached session to the attached client's 173 columns and clips the tab strip); CI has no attached client.

Round 4 (verify pass over the round-3 delta): zero open at P2 and above; eight P3s, all fixed in `eafe0faf..a56c7c0c` (three commits): the question is matched inside the picker block (our own echoed question higher up no longer vouches for a later picker), a picker with its question scrolled off still counts as open, the settle rule is a pure table-tested function, a late close is a delivery rather than a false timeout, an unreadable pane refuses the pick instead of typing blind, the in-flight answer map is cleared with the link, the help pairing guard now fails the row it was written for.

Round 5 (final allowed): verify pass over the round-4 delta.
