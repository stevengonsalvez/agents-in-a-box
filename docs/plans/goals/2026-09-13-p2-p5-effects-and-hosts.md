# /goal P2-P5 are merged to v2: every side effect the state machine used to perform is an `Effect` the host executes, the remaining screen states and the hangar plugin host wiring live in `ainb-app`, and one JSON `AppState` fixture per screen renders identically through the ratatui host

— CONTEXT —
· Project: agents-in-a-box (ainb) desktop programme, slice 2, phases P2-P5 of the base spec `docs/plans/2026-09-04-desktop-shared-core-spec.md` ("Extraction plan" rows P2-P5, "Renderer contract" table rows `Effect` and `Intent`, "Testing strategy" rows Core and Parity). Programme DAG: `docs/plans/2026-09-12-desktop-programme.md`. Decisions D1-D18 in `docs/plans/2026-09-11-multi-surface-decisions-spec.md` are locked; D15 (plan B mirror) lands in W0-mirror after this, so keep `SectionId` naming and per-section versions untouched.
· Stack: Rust workspace under `ainb-tui/`. P1 (lane F, 2026-09-13) moved the service layer (P1a, PR #973), the component pure halves (P1b, PR #975) and the state machine with `Chord`, `Intent`, `dispatch`, the `CommandId` registry and the `RendererHost` seam (P1c) into `ainb-tui/crates/ainb-app`; `ainb-core` renders through `pub use` shims; the `renderer_free` guard test proves the crate depends on neither ratatui nor crossterm. Inventory of what moved and what stayed: `docs/plans/2026-09-13-p1-extraction-inventory.md`, section "P1c" lists the component halves that still wait (session_list, fleet_panel, new_session configure and pick_repo) and the side effects the reducer still performs directly.
· Current state: v2 CI is fully green; registered flakes #953 and #958 are being root-caused by lane C; four macOS tripwires are gated to Linux under #966; #976 pins a duplicate keymap row id. The hangar plugin (`ainb-plugin-hangar`) still reads status through its own `to_wire_session` reconstruction; lane C owns replacing that with `fleet/status` under #962, which must land before or with your P3 step, so coordinate on that file rather than both editing it.
· Working dir: the Orca worktree this session was launched in, branched from `origin/v2` after P1c merged.
· Constraints: PRs target `v2`, never `main`; every commit GPG-signed with `git -c gpg.format=openpgp -c user.signingkey=907EC78C72C6AFF6 commit -S`; one concern per commit, named paths, never `git add -A`; no em-dashes on any line you author; never name a third-party product as prior art; stage as four PRs in spec order (P2 sessions, P3 hangar host, P4 review, P5 the rest), each compiling with its tripwires green before the next opens; you own `ainb-app`, `ainb-core/src/{app,components,screens}`, `main.rs` and `ainb-plugin-hangar` host wiring; never touch `ainb-hangar-daemon`, `ainb-hangar-store`, `ainb-hangar-proto` or `ainb-fleet-core`; `Effect` is exactly the spec's enum (`AttachTerminal(SessionId) | Detach | Notify{title,body,kind} | OpenEditor(Path) | Clipboard(String) | OpenUrl`) plus whatever the inventory proves the reducer already performs, each addition named in the PR body; the reducer returns `Vec<Effect>` and performs none of them; the TUI host executes them after the store commit (D15 invariant: effects after commit); the CTS lock is regenerated exactly once in P3 and named in that PR; `docs/tui/keyboard-shortcuts.md` stays byte-identical; run `cargo test -p ainb-app -p ainb`, `cargo check --workspace --all-targets`, `cargo clippy --workspace -- -D warnings`, `cargo fmt --all -- --check` and the named tripwire jobs before each push, with `CARGO_INCREMENTAL=0` and low parallelism on this box; update the P2-P5 rows in the programme doc in the PR that flips each.
· Serialisation: P1c does NOT ship Serialize on AppState; 47 credential-bearing or private-content fields are reachable from it (#983). Do not add Serialize to any section or moved type in P2-P5; the mirror phase lands it behind the redaction layer.
· Audience: the Tauri host (D1-D3), which will execute the same `Effect`s through its own host; W0-mirror, which mirrors sections and effects to the desktop; T0-section, which adds section 20.

— SUCCESS CRITERIA (ALL MUST BE TRUE) —
1. No function under `ainb-app` performs a side effect the spec assigns to the host: a guard test in `ainb-app/tests/` fails if the crate's manifest gains a clipboard, browser-open, editor or terminal-attach dependency, and a source-walk test fails on any `std::process::Command`, tmux invocation or clipboard call outside an allow-list that the PR body justifies line by line; every former direct attach goes through `Effect::AttachTerminal`, and a behavioural test per effect kind asserts `dispatch` returns exactly that effect and mutates exactly the expected section version.
2. `session_list`, `fleet_panel`, `new_session` (configure, pick_repo), `daemons`, `git_view`, `code_review`, `inbox`, `session_recovery`, `skill_manager_screen`, `log_history_viewer` and `config` states live in `ainb-app` with their reducers, and the hangar plugin host reads `ui.state` and routes `plugin/handle_action` through `dispatch`; the sessions, hangar, review and remaining tripwire jobs are green on both platforms for each staged PR, and the CTS lock changed in exactly one commit.
3. Parity: one committed JSON `AppState` fixture per screen under `ainb-app/tests/parity/` renders through the ratatui host to a `TestBackend` text snapshot that is byte-identical before and after each staged PR (snapshots taken at the P1c merge commit and stored with the fixtures), and `cargo test -p ainb-app` runs every moved test plus these, with counts before and after in each PR body.
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
