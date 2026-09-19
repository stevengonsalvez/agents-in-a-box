# /goal P6e makes the daemon's sessions table the one source for every surface at once: the TUI's readers and writers move onto `SessionSource` together with the CLI, the file rows created while P6d was dark are reconciled into the table without bringing back a row the table deleted, the `hangar.workspace.sessions` capability is advertised as the last commit, and a scripted `p6-concurrent` scenario proves a session created on one surface is seen by the other two, so slice 2 closes with one session store and no split brain

─ CONTEXT ─

· First act in the worktree: the checkout sits on `v2`; run `git switch -c p6e-sessions-one-source` before any edit, then copy this file to `docs/plans/goals/2026-09-19-p6e-sessions-one-source.md` and commit it as the first signed commit (`git -c gpg.format=openpgp -c user.signingkey=907EC78C72C6AFF6 commit -S`). Every later file change is its own signed commit; never `git add -A`. Push the branch and open each PR as a draft against `v2`, then report to the orchestrator by putting the PR number and head sha in the PR body's first line and in a PR comment; the orchestrator brings the verdict.

· Gate to start: P6d (#1206, branch `p6d-sessions-daemon`) is merged on `v2`. Every anchor below marked "P6d" is on that branch at `2e03c8d8e` and must be re-checked on `v2` after the merge; line numbers can move, names do not.

· Project: agents-in-a-box (ainb) desktop programme, slice 2 node P6, its last step.
  - Programme row: `docs/plans/2026-09-12-desktop-programme.md:126`, "P6 client reconnect, web onto client, sessions.json to daemon ... web e2e green; TUI + web + CLI concurrent smoke". P6a to P6d do not flip it; P6e does.
  - Parent goal: `docs/plans/goals/2026-09-16-p6-client-web.md`. Criterion 3 (`:131`), the no-destructive-migration constraint (`:121`) and the open questions (`:235-237`) are the ones this node finishes. P6d's PR body records which clauses of criterion 3 landed dark and which moved here.
  - Base spec `docs/plans/2026-09-04-desktop-shared-core-spec.md`: durable state moves into the daemon, sessions first (`:286`); the `sessions.json` audit row (`:318`); the concurrency gate that must pass on PR after P6 (`:355`); the migration-path open question (`:384`).

· Stack: Rust workspace under `ainb-tui/`; the daemon store is SQLite behind `sqlx` with numbered migrations under `ainb-tui/crates/ainb-hangar-store/migrations/` (P6d adds `0101_sessions.sql` and `0102_session_import.sql`); the proof harness is `ainb-tui/scripts/proof/run.sh`; the desktop journey is `ainb-tui/crates/ainb-desktop/e2e/specs/journey.e2e.js` in `.github/workflows/desktop.yml`.

· What P6d lands, dark. None of it is reachable from a release build until this node flips the capability.
  - The table and its typed repo: `ainb-hangar-store/src/repo/sessions.rs`, with `SessionsRepo::upsert` (`:186`) as `INSERT ... ON CONFLICT(session_id) DO UPDATE` in an IMMEDIATE transaction that refuses a tmux name bound to another id (`UpsertOutcome`, `:13`), `list` with a limit (`:111`), `get_by_tmux_name` (`:162`), `delete_by_id` (`:256`), the import marker `import_marker` (`:278`), `any_import_completed` (`:299`) and `complete_import` (`:315`) writing rows and the marker in one transaction.
  - The wire: `ainb-hangar-proto/src/methods.rs:30`, `:36`, `:42` (`workspace/session_list`, `workspace/session_upsert`, `workspace/session_delete`); `src/sessions.rs` with `WorkspaceSessionEntry::validate` (`:174`), `SESSION_LIST_MAX` (`:107`), and `truncated` (`:66`) and `import_complete` (`:71`) on the list result.
  - The capability, defined and dark: `ainb-hangar-proto/src/protocol.rs:181` `CAP_WORKSPACE_SESSIONS`, deliberately absent from `CAPABILITY_CATALOGUE` (whose last entry is `CAP_HOST_IDENTITY`, `:337`) and from `capabilities.catalogue`; pinned off by `the_workspace_sessions_capability_is_dark` (`:359`).
  - The daemon: handlers at `ainb-hangar-daemon/src/rpc/mod.rs:13457`, `:13489`, `:13513`, dispatched at `:1627-1629`; hello builds its list through `advertised_capabilities` (`rpc/auth.rs:415`, used at `:434`) with the test-only switch `advertise_workspace_sessions_for_tests` (`:409`); the boot import `session_import.rs:60`, called at `lib.rs:944`, capped at `SESSIONS_JSON_MAX_BYTES` (`:42`), whose module doc (`:20`) records the reconciliation this node owes.
  - The daemon's own registration: `run_loop.rs:1900` writes the file through `register_session` exactly as v2 does, then `:1903` shadow-writes the table through `shadow_write_session` (`:1718`) under `SHADOW_WRITE_TIMEOUT` (`:1710`), best-effort and only logged. `AinbSessionRecord::with_launch` (`ainb-fleet-core/src/fleet/session_registry.rs:100`) carries the real agent type, bypass flag and model into both.
  - The resolver: `ainb-app/src/cli/util.rs:108` `SessionSource { Daemon, File }`. `resolve` (`:127`) returns `File` without dialing while this build does not advertise the capability; `resolve_at` (`:141`) is the test seam; `resolve_with` (`:145`) requires the hello capability AND `import_complete`; `load` (`:180`) and `mutate` (`:221`) never fall back to the file once `Daemon` was chosen, write only the changed sessions, hold the `sessions.json` lock across the daemon read-modify-write, and save the file only on change; the process-wide choice is `SESSION_SOURCE` (`:280`) behind `session_source` (`:283`); the sync entry points are `load_session_store` (`:325`) and `mutate_session_store` (`:346`), both through `run_async` (`:294`).
  - The ten CLI sites already go through that resolver (`ainb-core/src/cli/list.rs`, `recover.rs`, `run.rs`, `status.rs`, `git_cmd.rs`). So does `ainb-web`, indirectly: it shells out to `ainb --format json list --frame` (`ainb-web/src/data.rs:346`).
  - Tests that drive the daemon path through the switch: `ainb-core/tests/session_cli_daemon.rs` (12), `ainb-hangar-daemon/tests/it_session_import.rs` (8), `ainb-hangar-store/tests/sessions.rs`.

· What is still on the file on `v2`, and is this node's blast radius. Every line below is on `origin/v2` at `db5167121`.
  - TUI and desktop reads (the desktop embeds `ainb-app` state, so it reads through the same code): `ainb-app/src/app/state.rs:3110` (stopped sessions into workspaces), `:5259`, `:5665`, `:9837`, `:10045`, `:13238`, and the locked read at `:13392-13395`; `ainb-app/src/components/session_recovery.rs:665`; `ainb-app/src/interactive/session_manager.rs:1927`, `:2392`, `:2468-2473`; `ainb-app/src/app/snapshot.rs:36`.
  - TUI and desktop writes: `ainb-app/src/interactive/session_manager.rs:1035` (purge a failed launch), `:1046` (`persist_codex_thread_id`), `:1575` and `:1846` (create); `ainb-app/src/components/session_recovery.rs:988` (recovery re-register) and `:1425`; `ainb-app/src/config/persist.rs:38` (`Persist::SessionHeadroom`, a compare-and-set inside the closure); `ainb-app/src/app/state.rs:13484`.
  - The snapshot copies the file verbatim: `ainb-app/src/app/snapshot.rs:92-95`.
  - The flocked file writer both paths share: `ainb-fleet-core/src/fleet/session_registry.rs:100` (`register_session`), `:168` (`lock_sessions_store_at`), and `SessionStore::lock` / `mutate` in `ainb-app/src/interactive/session_manager.rs:1209`, `:1229`.
  - The proof harness: `ainb-tui/scripts/proof/run.sh:86-92` is `ALL_NODES` (twenty nodes, ending `d1-shell`); `ainb-tui/scripts/surface-combo-smoke.sh:10` names its four combinations `{tui} {web} {tui,web} {tui,tui}`.
  - The desktop journey seeds a session with the real `ainb run` (`ainb-desktop/e2e/world.js:150`) and waits for it in the sidebar (`e2e/specs/journey.e2e.js:146`). On P6d's head `1cf133981`, where the capability was advertised, that step failed: `ainb run` wrote the table only while the sidebar read the file. That run is the concrete evidence for why the CLI and the TUI must flip in the same commit.

· Locked decisions. A node PR does not reopen one.
  - The daemon owns durable session state (base spec `:71`, `:286`, `:318`). After this node the table is the only store every surface reads and writes.
  - One source per process, decided once (P6d, `SessionSource` in a `OnceCell`), and a daemon failure after the decision is an error, never a silent switch to the file.
  - The import is one-time per file, keyed by the marker row; a failed import writes no marker and keeps clients on the file (P6d `session_import.rs`).
  - The RPC boundary validates every entry and never evicts a session that holds a tmux name (P6d `sessions.rs:174`, `repo/sessions.rs:186`).
  - No destructive migration: the import only reads `sessions.json`; a user who downgrades still has their sessions (parent goal `:121`).
  - The concurrency gate is a scripted scenario under `ainb-tui/scripts/proof/scenarios/` registered in `ALL_NODES`, never a hand test (parent goal constraints).

─ THE SEAM ─

```
             before (v2 and P6d)                       after P6e
┌──────┐ ┌─────────┐ ┌──────┐ ┌────────┐     ┌──────┐ ┌─────────┐ ┌──────┐ ┌────────┐
│ TUI  │ │ desktop │ │ CLI  │ │ daemon │     │ TUI  │ │ desktop │ │ CLI  │ │ web    │
└──┬───┘ └────┬────┘ └──┬───┘ └───┬────┘     └──┬───┘ └────┬────┘ └──┬───┘ └───┬────┘
   │ file     │ file    │ File   │ file         └──────────┴─────────┴─────────┘
   ▼          ▼         ▼        ▼+table(shadow)          SessionSource::Daemon
 ┌──────────────────────────┐                        ┌──────────────────────────┐
 │ sessions.json (flock)    │                        │ daemon sessions table    │
 └──────────────────────────┘                        └──────────────────────────┘
```

· The whole seam is `SessionSource`. P6e does not add a second resolver: every `SessionStore::load`, `lock` and `mutate` call in the list above becomes `load_session_store` / `mutate_session_store` (or their async forms), so the TUI, the desktop, the CLI and, through `ainb list --frame`, the web read one decision per process.
· The flip is a one-line change: append `CAP_WORKSPACE_SESSIONS` to `CAPABILITY_CATALOGUE` and to `capabilities.catalogue`. It is the LAST commit of the node, after every reader and writer has moved and the reconciliation has landed, so no intermediate commit on `v2` has a surface on the table while another is on the file.
· Reconciliation happens in the daemon, at boot, before the capability can matter to a client, in the same shape as the P6d import: a marker row, one transaction, and clients stay on the file until it is complete.

─ WHAT P6E MUST NOT DO ─

· No partial flip. No commit on `v2` may advertise the capability while any reader or writer in the blast-radius list is still on the file. The desktop journey failure on `1cf133981` is what that looks like.
· No resurrection. The reconciliation never re-inserts a session the table deleted, and never overwrites a table row with an older file row.
· No whole-file mirror. Nothing writes the daemon's snapshot over `sessions.json` (the P6d review removed exactly that); if the file is kept for downgrades, it is written row by row under its flock, never replaced wholesale.
· No second resolver, no per-call source decision, and no fallback to the file after the process chose the daemon.
· No edits to `ainb-tui/crates/ainb-core/src/app/*`, the standing lane rule. `ainb-app/src/app/state.rs` is not under that rule.
· No new wire field without the key-path fixture and bindings regenerated in the same PR (parent goal constraints), and `tests/serialize_guard.rs` with its fixture stays untouched: convert enums by name, as P6d's `metadata_to_entry` does.

─ SUCCESS CRITERIA (ALL MUST BE TRUE) ─

1. One source for every surface. With the capability advertised, `git grep -n "SessionStore::load\|SessionStore::mutate\|SessionStore::lock" -- 'ainb-tui/crates/ainb-app/src' 'ainb-tui/crates/ainb-core/src'` returns only the `SessionSource::File` implementation in `cli/util.rs`, `SessionStore`'s own definition and tests, and the snapshot follow-up if it is filed rather than moved. A test fails if a new direct call is added: a tripwire in the shape of `tests/serialize_guard.rs` with a committed call-site fixture.

2. The TUI sees what the CLI writes and the reverse. With a real daemon in a private hangar home (capability on, import complete): a session created by `ainb run` appears in the TUI's workspace list read (`state.rs:3110` path) without a restart; a session the TUI creates (`session_manager.rs:1575` path) appears in `ainb list --format json`; `ainb kill` of a TUI-created session removes it from the TUI's next read; `Persist::SessionHeadroom` keeps its compare-and-set semantics through the daemon (a test where the expected value moved leaves the row unchanged). Each is an integration test that fails when the corresponding site is reverted to the file.

3. Reconciliation without resurrection. A daemon boot on a home whose P6d import marker exists reconciles `sessions.json` once more, behind its own marker kind: every file session whose id is absent from the table AND not recorded as deleted is inserted; a session deleted from the table after the P6d import stays deleted; a table row newer than its file row is not overwritten; a second boot reconciles nothing; a failed reconciliation writes no marker and keeps `session_list.import_complete` false. Five tests, one per clause, each red when its guard is removed.

4. The flip is last and alone. The final commit of the implementation PR touches only `protocol.rs` (the catalogue entry), `capabilities.catalogue`, and the test that pinned the capability off (now pinning it on). `hello` advertises `hangar.workspace.sessions` in a test against a real daemon, and `SessionSource::resolve` answers `Daemon` with no test-only switch.

5. The desktop journey stays green with the capability on: `ainb-tui/crates/ainb-desktop/e2e/specs/journey.e2e.js`, including "the session created by the CLI reached the sidebar", in the `Desktop journey (ubuntu-latest)` job on the implementation PR's head, with the run id on the PR body.

6. The concurrent proof passes. `ainb-tui/scripts/proof/scenarios/p6-concurrent.sh`, registered in `run.sh`'s `ALL_NODES`, starts a TUI, an `ainb web` and a CLI against one daemon in every combination the scenario defines, creates a session from each surface and asserts the other two see it, kills one from each surface and asserts the other two drop it, answers an ASK from one surface and asserts the other two fold it, and writes `result.json` with `pass: true`. `bash ainb-tui/scripts/proof/run.sh --only p6-concurrent` passes on the PR's head, and a full harness run reports every other node passing.

7. No downgrade loss. A session created after the flip is still listed by the previous release (`ainb list` built from the P6d merge commit, no daemon) run against the same home, or the PR body states the decision that it is not and the orchestrator has accepted it on the PR (open question 2).

8. The programme row `docs/plans/2026-09-12-desktop-programme.md:126` is flipped to done with the PR numbers and the proof run id, in the implementation PR.

─ WHICH EXISTING TESTS MUST STAY GREEN ─

· `ainb-tui/crates/ainb-core/tests/session_cli_daemon.rs`, `ainb-hangar-daemon/tests/it_session_import.rs`, `ainb-hangar-store/tests/sessions.rs`, `migration_upgrade_full_chain.rs` and `tripwire_migrations_apply.rs`.
· The session-persistence tests that pin the file today: `ainb-core/tests/behavioral/session_persistence.rs`, `tests/orphan_session_removal.rs`, `tests/tripwire_plain_checkout_workspace_name.rs`, `tests/tripwire_stopped_session_keeps_its_label.rs`, `ainb-hangar-daemon/tests/tripwire_ccc_interactive_session_visible_to_fleet.rs` with `tests/tripwire_support/mod.rs:368`. Each keeps passing against the file path, or is converted to the daemon path in its own commit with the reason.
· `ainb-app/tests/host_side_effects.rs`, `serialize_guard.rs` with its fixture untouched, `state_serde.rs`, `bindings.rs`.
· The `session_manager.rs` pu4 concurrency tests (`:4817` onward on `v2`), converted to run against the resolver if the sites they pin move.
· CI: fmt, Contracts, Test (ubuntu, macos), ainb-core tripwires, hangar-e2e, web e2e, and `.github/workflows/desktop.yml` including the Desktop journey.

─ SCOPE, STAGED AS PRs ─

**P6e-1, reconciliation (daemon only, capability still dark).**
· Touches: a new migration after `0102` if a marker kind or tombstone table is needed, `repo/sessions.rs`, `session_import.rs`, `lib.rs`'s boot call.
· Proof: the five tests of criterion 3.

**P6e-2, every reader and writer onto `SessionSource`, then the flip.**
· Touches: the `ainb-app` sites listed under "still on the file", the call-site tripwire and its fixture, the pu4 tests, and as its final commit `protocol.rs` plus `capabilities.catalogue`.
· Proof: criteria 1, 2, 4, 5 and 7.

**P6e-3, the concurrent proof and the programme row.**
· Touches: `ainb-tui/scripts/proof/scenarios/p6-concurrent.sh`, `run.sh`'s `ALL_NODES`, `docs/plans/2026-09-12-desktop-programme.md:126`.
· Proof: criteria 6 and 8. May be folded into P6e-2 if the orchestrator prefers one PR for the flip and its proof.

─ CONSTRAINTS ─

· PRs target `v2`, drafts, one file per commit, every commit GPG-signed with the key above. Stage by named path.
· No attribution trailers, no `Co-Authored-By`, no mention of an AI model or an assistant in any commit message, PR body or PR comment. No em dashes on any line you author.
· `cargo fmt --all -- --check` before every push; `cargo clippy` on the touched crates with no new warnings in touched code.
· Never poll CI. Put the head sha in a PR comment; the orchestrator brings the verdict. Never merge, never flip a PR ready unless asked.
· Build with `CARGO_INCREMENTAL=0` and `-j 4`; keep the lane's target under 25 GB and the box under 85 percent; `cargo test -p <crate> --test <name>` over workspace builds.
· Never kill a tmux server or bulk-kill; exact session names and ports only.

─ OPEN QUESTIONS, EACH WITH A RECOMMENDATION ─

1. **How does the reconciliation know a row was deleted, not just missing?** Recommended: a `session_tombstone` table (session id, deleted_at) written in the same transaction as every `delete_by_id` / `delete_by_tmux_name` from the migration that adds it onward, and the reconciliation skips any id with a tombstone. While P6d is dark the only table deletes are the daemon's own retried-pane replacements (`shadow_write_session`), whose file entry was replaced the same way, so starting tombstones in P6e-1 loses nothing. Rejected alternative: comparing timestamps, because `SessionMetadata` has only `created_at` and no updated-at, so "newer" cannot be decided.

2. **Does a session created after the flip survive a downgrade?** Recommended: yes for one release. The daemon writes each changed table row into `sessions.json` through `register_session_at`'s flock (row-level upsert or remove, never a whole-file replace), best-effort and bounded like P6d's shadow write, and a later node removes it with a spec note. Rejected alternative: accept the loss, because the parent goal's constraint (`:121`) reads "a user who downgrades still has their sessions" without a date.

3. **Where does the reconciliation marker live?** Recommended: a `kind` column on `session_import` (`import`, `reconcile`) with the primary key widened to `(source_path, kind)`, in the new migration, so one table records both passes and `any_import_completed` can require both before `import_complete` is true.

4. **Do the reducer's synchronous reads block the TUI loop?** `state.rs` reads the store synchronously in reducers and effects; `load_session_store` goes through `run_async`, which uses `block_in_place` on the TUI's multi-thread runtime. Recommended: measure one `session_list` round trip on the proof box (expect single-digit milliseconds on the local socket), keep the sync entry points for the reducer sites, and move any site that runs per frame onto an effect that caches the store. Record the measurement in the progress log.

5. **What counts as "every combination" for `p6-concurrent`?** Recommended: the four combinations `scripts/surface-combo-smoke.sh:10` already defines, each with a CLI leg added, rather than a fresh matrix; the scenario reuses that script's fail-closed setup.

6. **The snapshot path (`snapshot.rs:92-95`) copies `sessions.json`.** Recommended: file it as a follow-up for the snapshots-index move base spec `:286` names next, and have the snapshot write the table's rows as `sessions.json` in the same format in the meantime, so a restore still has sessions.

─ FOLLOW-UPS TO FILE, NOT TO SOLVE ─

· Removing the downgrade file write (open question 2) once a release has shipped with the table authoritative.
· The thirteen fields spelled out in the repo row, the wire entry and the converters (P6d review, minor).
· Gating the sessions handlers on the capability server-side, together with the other `hangar.*` capabilities.

─ FINAL DELIVERABLE ─

Confirmation each criterion is satisfied. Every file created or modified. How to run, test and deploy. Proof (test output, the `p6-concurrent` result, the desktop journey run id). Decisions made and anything to know. Known limitations and follow-ups.

Begin by outputting your plan. Then execute end-to-end without checking in until done or genuinely blocked.

─ PROGRESS LOG ─

Plan, staged as the PRs above:
1. P6e-1: reconciliation with tombstones and its marker, capability still dark.
2. P6e-2: every `ainb-app` reader and writer onto `SessionSource`, the call-site tripwire, then the flip as the last commit.
3. P6e-3: `p6-concurrent` and the programme row.
