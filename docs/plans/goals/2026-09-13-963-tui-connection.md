# /goal #963 is closed on v2: a running TUI holds one live connection to the hangar daemon for its whole lifetime, `ainb hangar connections list` shows it as a `tui` row, and the S-D surface-combination smoke asserts that row positively

— CONTEXT —
· Project: agents-in-a-box (ainb) hangar daemon connection registry (S-B, on `v2`) and the TUI's daemon client. Issue #963 (two defects, one symptom): `DaemonClient::from_env` labels every client `cli` (S-D routed the TUI's twelve dial sites through `fleet::bridge::daemon::tui_client()`, so half one is done) and the TUI holds no connection at all, so the registry has nothing to list. Spec: `docs/plans/2026-09-11-multi-surface-decisions-spec.md` D10 (one daemon, surfaces are clients), S-B row, G6 step 6 in `docs/plans/2026-09-12-g6-human-checkpoint.md` (currently marked "known broken" because of this issue). Programme DAG: `docs/plans/2026-09-12-desktop-programme.md`.
· Stack: Rust workspace under `ainb-tui/`; registry `ainb-hangar-daemon/src/rpc/connections.rs` and `proto/connections.rs` (hello extension `{surface_kind, host, pid}`, presence lifecycle); client `ainb-hangar-client/src/lib.rs` (`from_env`, `set_surface`, hello); TUI dial sites `ainb-core/src/fleet/bridge/daemon.rs`, `attention_poll.rs`, `control.rs`; TUI entry `ainb-core/src/main.rs`; S-D smoke under `ainb-hangar-daemon/tests/` (the surface-combination test that expects a `tui` row and is gated today).
· Current state: `v2` at 526e71789 or later. Every TUI dial today is a short-lived connection per poll, so the registry sees `cli` rows flicker at best. The web surface writes its own hello frame in `ainb-web/src/daemon.rs` and is listed correctly. Lane F is concurrently moving side effects out of `ainb-core/src/app/*` into `ainb-app` behind an `Effect` outbox (P2, branch `stevengonsalvez/p2-effects`); the reducers must not learn about sockets.
· Working dir: the Orca worktree this session was launched in (`status-t0` on claude-gcp), branch `fix/963-tui-connection` already checked out from `origin/v2`; its build target was deleted, the first build is cold.
· Constraints: one PR targeting `v2`, small commits by named paths, commit with `git -c commit.gpgsign=false commit` (the orchestrator re-signs at merge) and say so in the PR body; never touch `ainb-core/src/app/*` or `ainb-app/*` (lane F owns them), put the connection lifecycle on the host side (`main.rs`, `fleet/bridge/daemon.rs`) as a long-lived task that dials once the daemon is reachable, holds the hello'd connection for the TUI's lifetime, reconnects with bounded backoff after a daemon restart, and closes on quit; the TUI connects at startup on every screen (the registry answers "which surfaces are live", a TUI parked on the home screen is live), which supersedes the "must be on the session list" note in G6 step 6, update that step and remove its "known broken" paragraph in the same PR; the existing per-call clients in `attention_poll.rs` and `control.rs` may keep their own short connections if sharing one would reshape them, but they must not register as separate presence rows (say how you prevented that); no new dependencies; gates: `cargo test -p ainb-hangar-daemon -p ainb-hangar-client -p ainb-core`, `cargo clippy --workspace -- -D warnings`, and the S-D smoke with its `tui` assertion made positive; merge `origin/v2` before touching any file lane F changed and rebase daily; if lane F's P2 PR 1 lands mid-way, adopt its `Effect` seam only if the host task can stay outside the reducers, otherwise leave a one-paragraph note in the PR for the P2 follow-up.
· Audience: operators reading `ainb hangar connections list`, G6 step 6, and the desktop host which will hold the same kind of connection.

— SUCCESS CRITERIA (ALL MUST BE TRUE) —
1. With an isolated `HOME` and `AINB_HANGAR_HOME`, a daemon from `hangar daemon setup`, and a TUI launched to the home screen, sampling `ainb hangar connections list` once a second for 20 seconds shows exactly one `tui` row every second (pid equal to the TUI's) and no `cli` row from the TUI; the row disappears within the registry's presence timeout after the TUI quits; the S-D surface-combination smoke asserts the `tui` row with no gate and is green three runs in a row.
2. Killing and restarting the daemon while the TUI runs yields a new `tui` row within 5 seconds and no panic or error toast in the TUI; the TUI started with no daemon running registers within 5 seconds of the daemon coming up.
3. G6 step 6 in `docs/plans/2026-09-12-g6-human-checkpoint.md` reads "one `tui` row and one `web` row" with the known-broken paragraph removed, and #963 is closed by the PR.
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
