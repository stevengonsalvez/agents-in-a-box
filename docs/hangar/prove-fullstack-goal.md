# GOAL: prove-fullstack. Real agents build a real app through the Hangar TUI

Drop this file into a fresh Claude Code session at the repo root and run it end to end
without hand-holding. It is the successor rung to `verify-converged-goal.md` (P11): that
harness proved the control plane with seeded fixtures and fake providers; this run proves
the product with REAL provider agents writing REAL code for a REAL app, with every
operator action driven and observed through the TUI. The operating surface is the TUI
(twice before, CLI+daemon green hid operator-visible bugs; see the seam-bug history in
`ainb-plugin-hangar/src/screen/fleet.rs` provider mapping and the chat-bus channel bugs).

## Decisions this run executes (do not re-litigate; redirect via Stevie only)

| # | decision | choice |
|---|----------|--------|
| 1 | app | mini ticket tracker "Boxtrack": auth, projects, tickets CRUD, kanban, activity feed |
| 2 | stack | Hono + SQLite + Vite/React, TypeScript both ends |
| 3 | app repo | throwaway private GitHub repo under stevengonsalvez; agents run `gh pr create` |
| 4 | phases | P1 happy path, P2 pipeline + squad, P3 live human loop, P4 levers + observability |
| 5 | roster | 3 claude sonnet agents (implementer/reviewer/tester) plus leader; max_concurrent_tasks=1 |
| 6 | fix scope | driven-path fixes (TUI + CLI parity + tripwire) plus docs refresh; dead-end RPCs become beads |
| 7 | commits | literal one file per commit, signed, via /commit; no AI attribution |
| 8 | budget | uncapped until green |
| 9 | CLI policy | CLI only where no TUI path exists (e.g. `pipeline init`); log as TUI gap, file bead |
| 10 | artifacts | `docs/hangar/proofs/fullstack/` gifs + stills + REPORT.md; record scripts in `scripts/hangar/` |
| 11 | launch gate | none parked; this doc is the first PR commit, Stevie redirects by message anytime |
| 12 | reporting | chapter gif per green leg only (failures never recorded) + PR body live leg table |

Reversibility rule: the app is a swappable workload pack (section "Workload pack" below).
Nothing in Phase 0 or the harness may depend on Boxtrack specifically. If you catch
yourself doing work that only pays off if the app choice was right, stop and say so.

## Mission

```
+---------+  +----------+  +----------+  +-----------+  +----------+  +--------+
| Phase 0 |->| P1 happy |->| P2 pipe  |->| P3 human  |->| P4 lever |->| report |
| harness |  | path     |  | + squad  |  | loop      |  | + obsrv  |  | + PR   |
| iso env |  | 1 issue  |  | 3 stages |  | live ASK  |  | walk     |  | polish |
+---------+  +----------+  +----------+  +-----------+  +----------+  +--------+
     fix anything broken on the driven path, per-file commits, re-drive, record green
```

A leg is GREEN only when a positive assertion (expected pixels in the pane, row in
sqlite, commits in git) AND a negative assertion (no placeholder, no prior-screen bleed,
no stale row: check timestamps) both hold within a deadline-bounded poll. Ground truth is
always cross-checked outside the TUI: `sqlite3 $ISO/.agents-in-a-box/hangar.db`, `git log`
in the app repo/worktrees, `gh pr view`.

## Hard safety rules (non-negotiable)

- NEVER `tmux kill-server`, `pkill tmux`, `killall tmux`, or wildcard kills. Kill only
  sessions you created, by exact name. Kill daemons only by the exact PID you captured.
- Never touch the primary `~/.agents-in-a-box` or the primary tmux server.
- Instance #2 never runs `atc setup`, `fleet runtime install`, `bridge install` (host-global).
- Never pipe cargo output (`| tail` eats the exit code); log to file, check `$?` AND content.
- No cargo build while a provider agent or vhs recording is live (earlyoom, 7.7GB box).
  Idle gate: `ps -eo comm | grep -cE '^(rustc|cargo|rust-lld|collect2)$'` returns 0.
- Do not record failures. Fix, re-drive, record the green run.

## Phase 0: harness (app-agnostic)

1. Build from THIS worktree (own target/, no clash):
   `cd ainb-tui && OPENSSL_NO_VENDOR=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 cargo build -j1 -p ainb --bin ainb -p ainb-hangar-daemon`
   then `bash scripts/build-plugins.sh` and verify `dist/plugins/hangar-tui/hangar-tui` is executable.
2. Provision the isolated home (fake HOME is the master switch; `AINB_HOME` alone is NOT
   safe, its semantics split three ways in code):

```bash
ISO=/home/claude/ainb-e2e-home
mkdir -p "$ISO/.agents-in-a-box/config" "$ISO/tmux"
ln -s /home/claude/.claude      "$ISO/.claude"       # real agent auth + transcripts
ln -s /home/claude/.claude.json "$ISO/.claude.json"  # if present
ln -s /home/claude/.gitconfig   "$ISO/.gitconfig"
ln -s /home/claude/.config      "$ISO/.config"       # gh auth
# onboarding ack: version MUST come from the binary, a placeholder re-runs the wizard
V=$("$BIN" --version | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1)
printf 'completed = true\ncompleted_at = "2026-09-01T00:00:00Z"\nversion = "%s"\n' "$V" \
  > "$ISO/.agents-in-a-box/config/onboarding.toml"
# notifyd install modal eats the first keypress: seed install.json prompt_dismissed
# under BOTH bases (AINB_HANGAR_HOME resolves before AINB_HOME in notifyd paths)
```

3. Env block for EVERY instance #2 process (daemon, TUI, CLI, vhs launch lines):

```bash
export HOME="$ISO"
unset AINB_HOME AINB_HANGAR_HOME TMUX TMUX_PANE   # unset TMUX or the primary server wins
export TMUX_TMPDIR="$ISO/tmux"                    # private tmux server
export AINB_BIN="<worktree>/ainb-tui/target/debug/ainb"
export AINB_HANGAR_DAEMON_BIN="<worktree>/ainb-tui/target/debug/ainb-hangar-daemon"
export AINB_PLUGIN_ROOT="<worktree>/ainb-tui/dist/plugins"
export AINB_HEADROOM_PORT=18787
export CLAUDE_PEERS_PORT=17899
export HANGAR_CLAUDE_OAUTH_TOKEN="$(claude setup-token 2>/dev/null || true)"  # bypass shared keyring
```

4. `"$AINB_BIN" hangar daemon start`, then status; socket at `$ISO/.agents-in-a-box/hangar.sock`.
5. TUI smoke: tmux session (own server) 180x50, launch
   `exec env -u AINB_HANGAR_HOME HOME=$ISO ... "$AINB_BIN"`, wait for home footer, press
   `g`, assert hangar chrome + Connected dot. Driving protocol: single-char keys, no
   Enter after nav keys, `poll_capture` 200ms with deadline, re-send nav keys every ~1.5s
   until the target screen appears, send-once for toggle/edge keys, `send-keys -l` for text.
6. Create the throwaway GitHub repo (`gh repo create stevengonsalvez/boxtrack-proving --private`),
   clone it under `$ISO/projects/boxtrack`, seed an empty README on main.
7. Record-script skeleton in `scripts/hangar/record-fullstack-*.sh` following
   `record-control-center.sh`: mktemp nothing (reuse $ISO), bake env into the vhs Type
   line, `VHS_NO_SANDBOX=true`, terminate on `Wait+Screen` patterns kept OUT of typed
   commands, check every artifact size/mtime, regenerate gif from mp4 via ffmpeg palette
   if the gif encoder hangs.

## Workload pack: Boxtrack (the swappable section)

Issues created through the TUI wizard (`1` then `c`), each with brief + acceptance
criteria + repo `@$ISO/projects/boxtrack` + source branch main. Order and dependencies
via board cards (`w` depends-on). Suggested split, adjust freely:

1. Scaffold: Hono API + Vite/React workspace, npm scripts, vitest + playwright wiring.
2. DB layer: SQLite schema users/projects/tickets/events + migrations.
3. Auth: register/login endpoints + session middleware + tests.
4. Tickets API: CRUD + state transitions + activity events + tests.
5. Projects API: CRUD + membership + tests.
6. Web shell: routing, login page, project list.
7. Kanban board UI: columns by ticket state, drag to transition.
8. Activity feed UI + polish.
9. E2E smoke: playwright journey register -> create project -> ticket -> move -> feed.

Each issue's acceptance criteria include: tests pass headlessly, `npm run check` green,
commits pushed on the task branch, PR opened via `gh pr create` (agents' briefs say so).

## P1: single-issue happy path (the spine)

Drive: `1 c` wizard (title, brief, repo via `@` roster, acceptance criteria), assign via
`a` picker, Run. Watch: frosted banner (agent, elapsed, tool calls), working chip,
transcript stream on `2` (5-lane taxonomy), Kanban `K` auto-move, issue promoted to
In Progress then Done. Assert: task row done with result JSON (content, exit_code),
`task.branch` recorded (commits ahead of base), run_history + task_usage rows, worktree
torn down (keep-if-dirty otherwise), started/succeeded comments on the issue thread,
PR badge + `o` opener once the agent's `gh pr create` lands. Record `p1-happy-path`.

## P2: pipeline + squad

Setup: `S` create squad "shippers", add impl-1/rev-1/qa-1 with roles
implementer/reviewer/tester (`A` wizard created them, provider claude, model sonnet);
`pipeline init` via CLI (logged TUI gap + bead). Drive: board card into the pipeline,
squad assign (`s`), Run. Assert per stage: role gate (only the matching role pulls), WIP
limits, one-owner, reviewer is a DIFFERENT agent than implementer (excludes_prior_agent),
`parent_task_id` chains stages, card advances one column per finished stage, issue held
in_progress until terminal. Reviewer/tester stage prompts set via
`pipeline stage-prompt`. Record `p2-pipeline`.

## P3: live human loop (never proven with a real agent through the TUI)

One issue dispatched in INTERACTIVE mode whose brief instructs the agent to raise an
AskUserQuestion (a real decision, e.g. "SQLite file path: app.db or data/boxtrack.db?").
Watch the ASK surface on Control Center `C` (gold card, circled options, "N need you"),
answer by digit, assert verified tmux delivery into the live session (the agent's next
turn acts on the picked option, checked in the session JSONL tool_result, not just a
receipt), row open -> answered in sqlite, board flips to 0 need you. A DELIVERED receipt
alone is a FAILURE per AGENTS.md. Record `p3-human-loop`.

## P4: levers + observability walk (while real work runs)

Levers: cancel `X` mid-run (process group dies, card/issue consistent, worktree
keep-if-dirty), retry `R` on a failed task (and the no-retry operator override; resolve
the doc/code contradiction while here), priority + assignee swap, deps blocking (`w`,
blocked run refused with reason), notify rules grid toggle, daemon config knob, workspace
create/switch (data isolation visible), skills attach/detach, profile tier cycle.
Observables: `D` health sparkline, `U` usage tokens/cost per agent (nonzero, matches
task_usage), `L` logs level filter, `I` inbox unread flow, `F` fleet rows for live
sessions, `y` activity timeline, Ctrl+P palette jump. Detach/reattach the TUI mid-run:
snapshot reconcile shows no lost state. Record `p4-levers` (may be several short gifs).

## Fix loop (the actual point)

Every defect hit on the driven path: root-cause fix (one guard where all callers route,
not a symptom patch), CLI parity when the seam is shared, tripwire or exhaustive
display-mapping test pinning the class, per-file signed commits via /commit, then
re-drive the leg and record. Off-path dead ends already known (Autopilots `a`/`e` no RPC,
workspace rename dropped): beads, not this PR. Docs refresh IS in scope:
`tui-keybindings.md` (add Fleet/Agents/palette/missing keys), stale Help overlay, R-retry
contradiction. Never mock the product to make a leg pass; never let unit tests stand in
for a driven run.

## Loop protocol

Run as a self-paced /loop. Each iteration: pick the next red/undone leg, drive it, on
green record + commit artifacts, update REPORT.md + PR body leg table, land fix commits
as they happen. Harness-tracked waits (agent runs) re-invoke on completion; long
fallback wake 1200s+. When hard-blocked after 2 distinct fix attempts on one defect:
file bead, park that leg, continue on others, surface the block in the PR body and the
next turn-end block. Uncapped until green.

## Reporting

`docs/hangar/proofs/fullstack/REPORT.md`: leg table (leg | PASS/SKIP evidence | recording
| ground-truth check), env disclosure (real provider claude sonnet, real GitHub repo,
which legs interactive vs headless), TUI gaps hit (with beads), fix commit index. PR body
mirrors the leg table and stays current. All recordings are of green runs only, named
`p<phase>-<slug>.gif` + numbered stills, mp4 kept beside gif.
