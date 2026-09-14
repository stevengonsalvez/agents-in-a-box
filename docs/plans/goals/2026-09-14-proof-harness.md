# /goal A repeatable proof harness runs every delivered programme node against a build of v2 tip and records expected versus observed per node: `ainb-tui/scripts/proof/run.sh` drives the TUI and daemon through tmux in an isolated HOME with real fixtures, writes one result per node with its capture, exits non-zero on any failure, and its first run is posted with the table and captures

— CONTEXT —
· Project: agents-in-a-box (ainb) desktop programme on `v2`. Programme DAG and node table: `docs/plans/2026-09-12-desktop-programme.md`. The delivered nodes today: Phase 1 keymap, S-A, S-B, S-C, Phase 2 versioned sections, Phase 3 UiState seal, S-D (slice 1); P1 to P4 (slice 2, PRs #973, #975, #982, #1000, #1008, #1021); W0-wire (#935), T0-daemon (#934, #978), #963 TUI presence (#998), #962 fleet panel on fleet/status (#1014), T0-section section 20 (#1019), #983 redaction layer (#1026). In flight and not to be proven yet: #1038, #1036, #1043.
· What exists to build on: `docs/plans/2026-09-12-g6-human-checkpoint.md` has the literal commands for steps 1 to 10; lane H's run of steps 1 to 7 is on PR #964 (comment 5652698463) and describes the fixture method that works: a private `HOME`, `AINB_HANGAR_HOME` and `TMUX_TMPDIR` under one tmp dir, `TMUX` unset, the TUI autostarts its own private daemon, an `ainb run --worktree` session whose "agent" is a bash loop named `claude` that prints `agent tick N` and traps SIGINT, ASK cards raised through the real hook path `ainb fleet atc hook --event PreToolUse --matcher AskUserQuestion`, a stub `headroom` on PATH serving `/health`, and the web side of step 7 done with the exact `POST /api/answer` body from `frontend/app.js`. The orchestrator's quick capture script (isolated HOME, `ainb init --format json` first so the wizard does not open, keys via `tmux send-keys`, `tmux capture-pane -e -p`) is at `/tmp/proof.sh` on this box and its captures under `/tmp/proof-captures`; it found that `hangar connections list` shows the TUI twice (#1040) and that the first-run hooks popup on the session list eats keys until Esc.
· Stack: Rust workspace under `ainb-tui/`; binary `ainb` and `ainb-hangar-daemon` built with `CARGO_INCREMENTAL=0 cargo build -j 4 -p ainb -p ainb-hangar-daemon` then `bash scripts/build-plugins.sh`; the worktree here (`proof-v2`) already holds a built target for v2 at ffeb666c, rebuild only when `git rev-parse origin/v2` moves. `ainb doctor --wire-shape` checks the redaction fixture. Rust tests are not the proof: the proof is the running binary observed through tmux.
· Current state: the status explainer (regenerated every supervision tick) shows per node "validated by" and today most rows say "not yet run: proof lane". Your output feeds it: the orchestrator copies `proof-out/` off this box with scp after each run.
· Working dir: `/home/claude/orca/workspaces/agents-in-a-box/proof-v2` on claude-gcp (detached at v2 tip; create branch `feat/proof-harness` from `origin/v2`). Disk: keep `df -h /` above 20 GB; clear `ainb-tui/target/debug/incremental` if it drops.
· Constraints: PRs target `v2`, never `main`, open as a draft; commit with `git -c commit.gpgsign=false commit` (the orchestrator re-signs at merge), one file per commit, one concern per commit, named paths, never `git add -A`, push after every commit; no attribution trailers and never mention any AI tool in a commit message; no em-dashes on any line you author; never name a third-party product as prior art; you own `ainb-tui/scripts/proof/**` and one paragraph in `docs/plans/2026-09-12-g6-human-checkpoint.md` pointing at the harness; do not change product code or tests (if a scenario fails because the product is wrong, record the failure with the capture and the issue number, do not fix it; if no issue exists, file one with the capture); never touch the real `~/.agents-in-a-box`, `~/.claude` or the box's tmux server (private socket `-L proof` under `TMUX_TMPDIR` only, kill by exact session name); do not read GitHub in a loop (secondary rate limits are shared).
· Audience: Stevie, who wants to see each node run and validated with proof, and the orchestrator's status page.

— SUCCESS CRITERIA (ALL MUST BE TRUE) —
1. `ainb-tui/scripts/proof/run.sh [--build] [--only <node>]` runs on this box against v2 tip and drives one scenario per delivered node listed above, each scenario in its own file under `ainb-tui/scripts/proof/scenarios/<node>.sh` with a one-line `EXPECT` and the observation logic, sharing a `lib.sh` for the isolated HOME, fixture session, daemon, headroom stub, hook-raised ASK, web answer and `capture` helper; it writes `proof-out/<node>/result.json` (`node`, `expected`, `observed`, `pass`, `issue` when a known failure, `capture` file names, `binary` version line, `started_at`), `proof-out/<node>/*.txt` and `*.ans` captures, and `proof-out/summary.json` plus `summary.md` (one table row per node); exit code is non-zero when any node fails; a second run in a row gives the same results (no leftover state, private daemon and tmux server stopped at the end, verified by `ps`).
2. The scenarios prove the node, not just that the binary starts: keymap override (`attach -> o`, enter no longer attaches), embed passthrough (ctrl+c reaches the fixture agent, ctrl+q detaches), help overlay versus `keymap list`, S-A two writers (both settings survive) and config search filtering, S-A one headroom proxy (pid unchanged while B runs, no second spawn), S-B connections registry (expect exactly one `tui` row plus `web` and `cli`; today records #1040 as a known failure with the capture), S-C answered card retires from web and from TUI with the `answered by` toast, T0-section (a fixture session in `waiting · hook · tier 0 · Ns` renders that tuple in the fleet panel from section 20, and the panel shows the failure story when the daemon is stopped), #962 (panel counts match `ainb fleet status`), #963 (TUI row present at once and gone after quit), #983 (`ainb doctor --wire-shape` matches the fixture and a token-shaped string in a session title never appears in `hangar connections list` or a frame), P2 to P4 (attach in place opens and releases through the effect: the preview pane shows the fixture agent's `agent tick` lines, `A` then ctrl+q returns the strip; the review screens open from ctrl+p by name), W0-wire and T0-daemon (a second `ainb` CLI call sees the same daemon: `hangar connections list` shows its own `cli` row and the TUI row, and `fleet status` reports the fixture session with the same tuple the panel shows).
3. The first full run against v2 tip is posted as a comment on the draft PR with `summary.md` inline, the per-node captures attached as a gist or as files under `proof-out/` copied to `/tmp/proof-out` on this box, every failing node named with its issue number, and the programme doc's G6 section gains one paragraph saying which steps the harness now covers and how to run it; CI is not required for this goal but the scripts pass `shellcheck` and run from a clean clone.
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
