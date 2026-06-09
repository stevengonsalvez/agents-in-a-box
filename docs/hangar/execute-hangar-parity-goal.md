# /goal Execute hangar-parity epic agents-in-a-box-e38: all 35 beads closed via per-bead TDD + tripwire e2e, scoped gates green, PR `feat/hangar-parity` open with user-visible proof

— CONTEXT —
· Project: Hangar — TUI-first multica replica inside agents-in-a-box (daemon + SQLite store + hangar-tui plugin over unix-socket JSON-RPC). This run executes the parity epic from the 2026-06-09 multica review (113 features mapped: 19 parity / 36 partial / 45 gap / 13 oos-by-design; full matrix at docs/hangar/parity-review.html). Epic agents-in-a-box-e38 holds the 35 beads — read them with `BEADS_DIR=<main-repo>/.beads bd list --parent agents-in-a-box-e38`; bead bodies carry per-item scope + source. Wave order: W1 = P0+P1 (13 beads, security/arch/core gaps + 2 resilience tripwires) → W2 = P2 label `verification` (5 tripwires) → W3 = P2 `feature-gap`+`architecture` (15) → W4 = P3 (2). Gate between waves; a wave is done only when its beads are closed and the full tripwire suite is green.
· Stack: Rust workspace — ainb-tui/crates/ainb-hangar-{core,daemon,proto,secrets,store}, plugins/hangar-tui (plugin-runtime v2, Content-Length JSON-RPC), sqlx/SQLite WAL (migrations 0001–0010, 16 tables), real-tmux tripwire e2e suite in ainb-hangar-daemon/tests/, beads tracker, per-phase Workflow engine for orchestration (build sequentially WITHIN a wave — beads share crate files; review in parallel; never one mega-run).
· Current state: main with PR #179 merged (Hangar v1: ~454 acceptance tests + 28 tripwires green; tripwire_full_e2e meta-count baseline 28 — bump it as you add tripwires, read the actual count from a failing run, never guess). Pre-existing tolerated failures (do NOT fix, do NOT let them block): docker::agents_dev_tests, fixture_e2e::send_key_*, handle_key_ordering clippy ×2, test_session_creation_refresh/behavioral, beads_adapter concurrency flake. docs/hangar/verify-hangar-goal.md is the product-verification harness — its R01–R08 resilience legs are IMPLEMENTED BY beads e38.25–32, so the final verify-walk must pass them for real.
· Working dir: PRECONDITION — verify PR #179 is merged (`gh pr view 179 --json state`); if not merged, STOP and report. Then from /Users/stevengonsalvez/.agents-in-a-box/repos/github.com/stevengonsalvez/agents-in-a-box: `git fetch && git worktree add ~/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_hangar-parity -b feat/hangar-parity origin/main` and work there; export BEADS_DIR=/Users/stevengonsalvez/.agents-in-a-box/repos/github.com/stevengonsalvez/agents-in-a-box/.beads for every bd call.
· Constraints: PRE-LOCKED DESIGN DECISIONS (do not re-litigate) — e38.2: WIRE real event-push emission (emit HangarEvent at every finalize/claim/scheduler transition; per-connection subscriber fanout in serve_conn; plugin decode path already exists in plugins/hangar-tui/src/stream.rs) and update architecture.md to match; e38.3: migrate to per-(issue,agent) concurrency parity (partial index → (issue_id, agent_id), port multica's NOT-EXISTS active-set guard into the claim SQL, migration + upgrade-from-populated test). GATES — scoped per-crate clippy only, NEVER `cargo clippy --workspace -- -D warnings`; rustfmt single-file only (`rustfmt <file>`), NEVER crate/workspace `cargo fmt` (rustfmt.toml is nightly-only; crate-wide fmt rewrites everything); tripwire rules — exact-name `tmux kill-session -t <name>` only (NEVER kill-server/pkill/wildcard), poll_capture not bare sleep, single-char nav keys re-sent per poll, positive AND negative markers, SKIP-not-fail, --test-threads=1, trim_end_matches('\n') before byte-asserts; per-bead TDD — claim bead (`bd update <id> --assignee --status in_progress`) → failing test first → implement → code-review subagent → fix same run → suites green → `bd close <id>`; commits atomic per concern by NAMED paths (never `git add -A`), human-authored, no AI/Claude mention, conventional format, merge-commit never squash; every feature bead needs at least one USER-VISIBLE proof (tmux capture or real CLI output), green tests alone are not acceptance; disclose mocked-vs-live for provider-touching tests (fake-claude = mocked); secrets never in code or commits; open the PR after the first wave-1 bead lands and roll subsequent work into it.
· Audience: Stevie (operator) + ainb end users driving the Hangar TUI/CLI.

— SUCCESS CRITERIA (ALL MUST BE TRUE) —
1. All 35 beads under epic agents-in-a-box-e38 are closed, each landed as atomic commits with its own acceptance and/or tripwire tests written test-first.
2. Scoped clippy clean on every touched crate + full hangar acceptance suite + the ENTIRE tripwire suite (including all new ones) pass 3× consecutively with --test-threads=1, with zero non-tolerated failures.
3. The docs/hangar/verify-hangar-goal.md walk passes end-to-end — F01–F44 plus R01–R08 all PASS (or justified greppable SKIP, e.g. keychain on CI) — and PR feat/hangar-parity is open with user-visible proof (tmux captures / CLI transcripts) in the description.
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
