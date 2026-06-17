# GOAL: verify-hangar — full autonomous product verification

Drop this file into a fresh Claude Code / Codex session at the repo root and run it
end-to-end without hand-holding. The outcome is a per-feature PASS/SKIP/FAIL table
covering every user-facing Hangar feature (F01–F44) plus eight resilience legs
(R01–R08) the static test suite does not cover. You are verifying the product the
way a sceptical user would — real binaries, real tmux, real SQLite — not re-running
unit tests.

## Mission

```
┌─────────┐   ┌──────────┐   ┌─────────────┐   ┌─────────────┐   ┌─────────┐
│ build + │──▶│ seed     │──▶│ walk F01-44 │──▶│ resilience  │──▶│ report  │
│ stage   │   │ fixtures │   │ CLI+TUI+    │   │ R01-R08     │   │ table + │
│ plugin  │   │ 2 ws     │   │ daemon legs │   │ kill/crash/ │   │ exit N  │
└─────────┘   └──────────┘   └─────────────┘   │ migrate     │   └─────────┘
                                               └─────────────┘
```

A feature is GREEN only when a **positive** assertion (seeded data rendered /
row persisted / bytes on disk) AND a **negative** assertion (no placeholder, no
prior-screen bleed, no leak, no cross-workspace bleed) both hold within a
deadline-bounded poll.

## Hard safety rules (non-negotiable)

- NEVER `tmux kill-server`, `pkill tmux`, `killall tmux`, or wildcard session kills.
  Kill ONLY your own session by its exact unique name: `hangar-verify-<pid>-<nanos>`.
- Kill the daemon ONLY by the exact child PID you spawned.
- NEVER `cargo clippy --workspace -- -D warnings`; never crate-wide `cargo fmt`.
- Per-feature `$HOME` tempdirs; never touch the real `~/.ainb`.

## Phase A — environment setup

1. `cargo build -p ainb -p ainb-hangar-daemon -p ainb-hangar-secrets` (release not
   required). Stage the plugin: `scripts/build-plugins.sh` → verify
   `dist/plugins/hangar-tui/` contains an executable binary; on macOS re-`touch`
   after any copy so AMFI does not SIGKILL a stale-signed binary (exit 137, no stderr).
2. `tmux -V` must succeed; `sqlite3 --version` must succeed. If either is missing →
   every tmux/daemon leg is `SKIP: <reason>` (greppable), never FAIL.
3. For every leg: fresh `HOME=$(mktemp -d)` with seeded `onboarding.toml` +
   first-run ack so the wizard/danger modal never intercept keystrokes. Resolve the
   expected version from `[workspace.package]` in the root `Cargo.toml` (the daemon
   crate's own version differs — using it re-fires the wizard).
4. Read `ainb-tui/crates/ainb-hangar-daemon/tests/tripwire_p4_common.rs` and reuse
   its seeding helpers (`seed_p4_fixture`, `seed_autopilot`, `seed_logs`) rather
   than re-inventing fixtures.

## Phase B — feature walk (F01–F44)

Protocol per surface type:

| Surface | How to verify |
|---------|---------------|
| CLI | Run the real `ainb hangar <verb>`; assert stdout AND the resulting DB row via `sqlite3 "$HOME/.ainb/hangar/hangar.db"` |
| TUI screen | Spawn `ainb` in tmux, press the documented key, `poll_capture` for a POSITIVE seeded marker AND assert a NEGATIVE (no `Loading`, no previous-screen marker). Re-press single-char nav keys every ~1.5s until the marker appears (first frames race session discovery). Add a return-navigation leg (key away, key back) so a one-way key swallow cannot pass |
| Daemon/control-plane | Run the real daemon binary against the seeded DB; assert terminal DB state or on-disk bytes (byte-equality with `trim_end_matches('\n')`) |

Markers must be collision-proof and greppable (`LOGS_TRIPWIRE_MARKER_42`,
`#<short_id>` suffixes) — never chrome strings like "Issues" or "Tasks".

The checklist (ui key / CLI from `docs/hangar/architecture.md`, verified 2026-06-09):

| id | Feature | Drive via |
|----|---------|-----------|
| F01 | Issue create/list/show | CLI `ainb hangar issue create\|list\|show` |
| F02 | Issue + assignee persisted | CLI create then sqlite3 row assert |
| F03 | Issue list screen (nav/filter/create) | TUI `1`, `j/k`, `/`, `c` |
| F04 | Kanban board + card move | TUI `K`, arrows, `Shift+←/→` then DB transition assert |
| F05 | Task FSM claim→terminal | daemon + fake-claude happy path |
| F06 | Retry chain parent/child cap | CLI `task retry` + DB parent_task_id chain assert |
| F07 | TTL sweepers | daemon with stale-seeded rows → failed |
| F08 | Task detail live transcript | TUI `2`/Enter; poll transcript lines |
| F09 | Task CLI list/cancel/retry | CLI |
| F10 | Task-started banner | TUI: dispatch then poll banner text |
| F11 | Agent picker assign | TUI `a`, Enter; DB assignee assert |
| F12 | agents_list snapshot | covered inside F11 leg |
| F13 | Skill CRUD scoping/cascade | CLI skills + sqlite3 cascade assert |
| F14 | Skills sync idempotent | CLI `skills sync` twice; row count stable |
| F15 | Skill manager screen | TUI `4`, `s/i/d`, chips |
| F16 | Dispatch-time skill materialisation | daemon dispatch; byte-assert materialised SKILL.md |
| F17/F18 | Templates list/show/use | CLI `templates ...` |
| F19 | Autopilot CRUD + invalid-cron reject | CLI create (good + bad cron) |
| F20 | Scheduler fires on schedule | daemon, near-future cron, poll autopilot_run row |
| F21 | Scheduler skips when running | daemon, in-flight seeded run |
| F22 | Autopilots screen | TUI `5`; seeded `daily-triage` marker |
| F23 | Autopilot CLI run-now | CLI `autopilot run` |
| F24 | Keychain roundtrip | mac-only; SKIP on CI/linux |
| F25 | secret_store_get cap gating | ungranted manifest → expect -32001 in plugin log |
| F26 | PAT/daemon tokens hash-only | CLI `auth token create\|list\|revoke`; assert sha256-only in DB |
| F27 | Env allowlist blocks LD_PRELOAD | daemon dispatch env assert |
| F28 | danger-warning first-run | TUI fresh HOME, `y` ack; `config warnings reset` re-fires |
| F29 | Workspace switching | TUI `,` + `s`; THEN assert issue list actually shows ws-B's distinct issues (see R03) |
| F30 | Settings screen + key entry | TUI `,`, `n` |
| F31/F33 | JSONL sink + spans | run daemon; grep daemon.jsonl for span names |
| F32 | OTLP export | feature-gated; SKIP unless `--features otlp` build requested |
| F34 | Daemon health pane + sparkline | TUI `D` |
| F35 | Logs tail CLI + screen | CLI `logs tail`; TUI `L` + level chips |
| F36 | PR-URL capture | daemon; fake-claude emits PR URL; DB assert |
| F37 | PR badge + `o` open | TUI badge render (do NOT actually open browser in CI) |
| F38 | Daemon boot + migrations | run daemon on fresh dir; 16 tables assert |
| F39 | Socket RPC + snapshots | plugin connects to real socket |
| F40 | subscribe + daemon-drop detection | kill daemon (exact PID); plugin shows disconnect state |
| F41 | Cross-screen nav | TUI `1/2/4/5/K/D/L/,` + `?` + `q` |
| F42 | Beads sync reconcile | CLI `beads reconcile --dry-run` against seeded bd |
| F43 | Runner env/exit/stream/timeout | inside F05 leg + a short-timeout task |
| F44 | Meta-guard | run `tripwire_full_e2e` once as the static-suite sanity anchor |

## Phase C — resilience legs (the quadrant the static suite omits)

| id | Leg | Pass condition |
|----|-----|----------------|
| R01 | Daemon kill-9 mid-task, restart same DB | WAL survives unclean kill; orphaned `dispatched/running` row reaches a terminal state via sweep on restart |
| R02 | Plugin crash → host re-dial; host exit → no orphan | after host exit: `ps -Ao pid,ppid,command \| grep 'hangar-tui/hangar-tui' \| awk '$2==1'` is EMPTY (parent-death watcher, commit 8494ad0f) |
| R03 | Multi-workspace DATA isolation | two NON-EMPTY workspaces with distinct issues; switching changes visible issues; ws-A markers never appear under ws-B |
| R04 | Migration upgrade-from-populated | apply 0001..N-1, seed real rows, apply N; rows intact; double-apply idempotent |
| R05 | Concurrent dispatch respects cap | N=8 queued, cap=2: never >2 running, no double-claim (DB audit), no WAL deadlock |
| R06 | End-to-end retry + timeout | fail task (infra reason) → child spawned, capped at max_attempts; `agent_error` does NOT retry; timeout task reaches failed |
| R07 | Create-flow keystroke round trip | `c` create issue, `a/e` create autopilot, `Shift+→` card move: each keystroke → RPC → DB row → re-render assert (not SQL-seeded) |
| R08 | Short soak | 5 min daemon+plugin under steady fake-claude transcript stream; RSS of both processes stable (±20%), no dropped-frame errors in logs |

## Flake handling

- No bare `sleep` before any capture — always `poll_capture(deadline, predicate)`
  with ~200ms gaps.
- Single-char nav keys WITHOUT Enter; re-send each poll until marker appears.
- Serialize everything (`--test-threads=1` semantics): one daemon, one tmux
  session at a time — socket binds and env mutation race across processes.
- SKIP-not-fail with a greppable `SKIP: <leg> <reason>` when tmux/binaries/
  keychain are absent.

## Teardown (every leg, even on failure)

1. `tmux kill-session -t "$SESSION"` (exact name only).
2. `kill <exact-daemon-pid>`; escalate to `-9` only after 5s grace.
3. Assert NO orphaned `ainb-hangar-daemon` or `hangar-tui` process survives
   (PPID=1 grep above) — this doubles as the watcher regression check.
4. `rm -rf` the per-leg `$HOME` tempdir.

## Reporting

- Emit one table: `leg id | name | PASS/SKIP/FAIL | evidence (1 line)` for
  F01–F44 + R01–R08.
- Exit code = number of FAILed legs. On any FAIL print the last 20 lines of the
  relevant capture/log.
- DISCLOSE prominently: provider execution is **mocked via fake-claude.sh**, not
  the live claude binary (per the mocked-vs-live disclosure rule). Note any leg
  verified at daemon level rather than through tmux.
- Do not modify product code. If a leg fails, file the evidence — do not "fix"
  the product mid-run.
