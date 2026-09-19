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
  - One source per process, decided once (P6d, `SessionSource` in a `OnceCell`), and a daemon failure after the decision is an error, never a silent switch to the file. P6e adds exactly one transition, `Degraded` to `Daemon` ("Daemon down at startup").
  - The import is one-time per file, keyed by the marker row; a failed import writes no marker and keeps clients on the file (P6d `session_import.rs`). The reconcile that follows it is repeatable ("Mixed versions"); only the first import is one-time.
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
· Kill switch: `AINB_SESSION_SOURCE=file` in a process's environment makes `resolve` answer `File` before it dials anything: no `Degraded` notice, no re-resolve, no RPC. The flip is a compile-time constant, so this is the rollback that does not need a re-release. Any other value is ignored with one warning. Because the file stays written ("Mixed versions"), a process forced onto the file still sees current sessions.
· Reconciliation happens in the daemon, in the same shape as the P6d import (a marker row, one transaction, clients on the file until the first pass completes), but it is repeatable, not one-time: see "Mixed versions" below.

─ DAEMON DOWN AT STARTUP ─

· On P6d, `resolve` answers `File` on any dial or hello failure (`util.rs:133`, `:148`) and `session_source` caches that answer for the life of the process (`util.rs:280`). A TUI or desktop started before the daemon would then write the file for hours while the CLI and the web read the table. P6e defines that case instead of inheriting it.
· A build that advertises the capability but cannot reach a daemon that advertises it (dial fails, hello fails, or `import_complete` is false) resolves to a third variant, `SessionSource::Degraded`:
  - Reads and writes go to the file under the flock, exactly as `File` does. Nothing is refused, so a user without a daemon keeps working.
  - A visible notice says sessions are on the local file until the daemon is up: a banner in the TUI and the desktop, one stderr line from a CLI command.
  - A long-lived process (TUI, desktop, `ainb web`) re-resolves on a bounded schedule, the P6a cadence of 1 s, 4 s, 16 s and then every 16 s, each attempt bounded by the RPC deadline. A CLI command does not re-resolve; it ends.
  - When a re-resolve reaches a daemon that advertises the capability, the surface asks it to reconcile (a new `workspace/session_reconcile` RPC in the workspace family) and waits for that pass to complete, then switches to `Daemon` for the rest of the process. The writes it made while degraded are in the file, so that pass inserts them.
· The only transition is `Degraded` to `Daemon`, once. There is no transition back: a daemon failure after the switch stays an error, as on P6d. This amends P6d's "decided once per process" rule for exactly this case, by the orchestrator's decision on #1210.

─ MIXED VERSIONS, AND WHY THE FILE STAYS WRITTEN ─

· `advertises` reads the client's OWN compiled catalogue (P6d `protocol.rs:342`), so a pre-P6e binary (a CLI, TUI or `ainb web` left running from the previous release, or a second install) always resolves to the file, whatever the daemon advertises. A pre-P6e daemon never advertises, so a post-P6e client beside it stays on the file too. Both cases must be safe, not just rare.
· The file stays written after the flip, so a downgrade sees current sessions. Every write through `SessionSource::Daemon` takes the `sessions.json` flock and changes the file row by row (upsert or remove by tmux key, the `register_session_at` shape), never a whole-file replace, and changes the table inside the same flock. Order: the file row first, then the table; if the table write fails, the file change is reverted before the flock is released and the error is returned. So after every successful new-binary write, the file and the table agree on that session.
· The reconcile is repeatable. It runs on every daemon boot, on a surface's move out of the degraded state (see "Daemon down at startup"), and whenever `sessions.json`'s mtime has changed since the last pass, checked at most every 30 s. Each pass inserts every file session whose id is absent from the table; the table wins on everything else. A tmux-name conflict is one of those: a file session whose id is new but whose tmux name the table already binds to another id is not inserted (P6d `upsert` refuses it) and is counted on the pass's marker as skipped, with one warning naming both ids, never silently dropped. Because every new-binary delete removes the file row first, a session that is in the file and not in the table can only have come from a writer that bypassed the table (an old binary, or a surface while degraded), so inserting it is correct and never resurrects a row the new stack deleted.
· Old-binary deletes are the stated limitation: a previous release's `ainb kill` removes the file row only, the table keeps the session, and no reconcile deletes. The new stack goes on listing that session until it is killed from a new binary. A test pins this so a change to the rule is deliberate.

─ WHAT P6E MUST NOT DO ─

· No partial flip. No commit on `v2` may advertise the capability while any reader or writer in the blast-radius list is still on the file. The desktop journey failure on `1cf133981` is what that looks like.
· No resurrection. The reconciliation never re-inserts a session the table deleted, and never overwrites a table row with an older file row.
· No whole-file mirror. Nothing writes the daemon's snapshot over `sessions.json` (the P6d review removed exactly that); the file is kept current row by row under its flock, as "Mixed versions" describes.
· No second resolver, no per-call source decision, and no fallback to the file after the process chose the daemon.
· No edits to `ainb-tui/crates/ainb-core/src/app/*`, the standing lane rule. `ainb-app/src/app/state.rs` is not under that rule.
· No new wire field without the key-path fixture and bindings regenerated in the same PR (parent goal constraints), and `tests/serialize_guard.rs` with its fixture stays untouched: convert enums by name, as P6d's `metadata_to_entry` does.

─ SUCCESS CRITERIA (ALL MUST BE TRUE) ─

1. One source for every surface. With the capability advertised, `git grep -n "SessionStore::load\|SessionStore::mutate\|SessionStore::lock" -- 'ainb-tui/crates/ainb-app/src' 'ainb-tui/crates/ainb-core/src'` returns only the `SessionSource::File` implementation in `cli/util.rs`, `SessionStore`'s own definition and tests, and the snapshot follow-up if it is filed rather than moved. A test fails if a new direct call is added: a tripwire in the shape of `tests/serialize_guard.rs` with a committed call-site fixture.

2. The TUI sees what the CLI writes and the reverse. With a real daemon in a private hangar home (capability on, import complete): a session created by `ainb run` appears in the TUI's workspace list read (`state.rs:3110` path) without a restart; a session the TUI creates (`session_manager.rs:1575` path) appears in `ainb list --format json`; `ainb kill` of a TUI-created session removes it from the TUI's next read; `Persist::SessionHeadroom` keeps its compare-and-set semantics through the daemon (a test where the expected value moved leaves the row unchanged). Each is an integration test that fails when the corresponding site is reverted to the file.

3. Reconciliation without resurrection, repeatable. Daemon state: capability on (test switch before the flip), P6d import row present. Each pass (boot, degraded exit, mtime change) inserts every file session whose id is absent from the table; a table row is never overwritten by its file row; a session deleted through the new stack is gone from the file too, so a later pass does not bring it back; a session an older binary appended to the file after the previous pass is inserted by the next one; a file session whose tmux name the table binds to another id is skipped and counted, and the table row is unchanged (table wins); a failed pass writes no marker and keeps `session_list.import_complete` false even though the P6d import row exists (the kind-aware check of open question 3). Six tests, one per clause, each red when its guard is removed. No tombstones: see open question 1.

4. The flip is last and alone. The final commit of the implementation PR touches only `protocol.rs` (the catalogue entry), `capabilities.catalogue`, and the test that pinned the capability off (now pinning it on). `hello` advertises `hangar.workspace.sessions` in a test against a real daemon, and `SessionSource::resolve` answers `Daemon` with no test-only switch.

5. The desktop journey stays green with the capability on: `ainb-tui/crates/ainb-desktop/e2e/specs/journey.e2e.js`, including "the session created by the CLI reached the sidebar", in the `Desktop journey (ubuntu-latest)` job on the implementation PR's head, with the run id on the PR body.

6. The concurrent proof passes. `ainb-tui/scripts/proof/scenarios/p6-concurrent.sh`, registered in `run.sh`'s `ALL_NODES`, starts a TUI, an `ainb web` and a CLI against one daemon in every combination the scenario defines, creates a session from each surface and asserts the other two see it, kills one from each surface and asserts the other two drop it, answers an ASK from one surface and asserts the other two fold it, and writes `result.json` with `pass: true`. `bash ainb-tui/scripts/proof/run.sh --only p6-concurrent` passes on the PR's head, and a full harness run reports every other node passing.

7. Mixed versions, both directions. Daemon state: a post-flip daemon up with import and reconcile complete, and a previous-release `ainb` built from the P6d merge commit. (a) New writer, old reader: a session created through the new CLI and one killed through it are, respectively, listed and not listed by the previous release's `ainb list` against the same home, and a new write whose table step is made to fail leaves the file unchanged. (b) Old writer, new reader: a session the previous release's `ainb run` creates appears in the new `ainb list` after at most one reconcile interval, and a session the previous release kills stays listed by the new stack, pinning the stated limitation.

8. Daemon down at startup. Daemon state: none when the TUI starts, then one started. A TUI started with no daemon resolves to `Degraded` and shows the notice; a session it creates lands in the file; a daemon is then started; within the re-resolve bound (at most 16 s after the daemon's hello succeeds, plus the reconcile) the TUI leaves `Degraded`, the session created while degraded is in the table, a CLI resolved to `Daemon` lists it, and a session the TUI creates after the switch is in both the table and the file. The test fails if the TUI stays on the file after the daemon is up, or if the degraded-time session is missing from the table.

9. The kill switch works. Daemon state: a daemon that advertises the capability with import and reconcile complete, behind a socket that counts accepted connections (the fake-daemon shape of P6d's `session_cli_daemon.rs`). With `AINB_SESSION_SOURCE=file` set, `SessionSource::resolve` answers `File` and the socket accepted zero connections; without it, the same setup answers `Daemon`. One test, red if the variable is read after dialing or not at all.

10. Bounded blocking, no nested lock. Daemon state: a daemon that answers hello with the capability and import complete, then never answers a session RPC (a fake socket). Every converted reducer and effect site returns an error within `SESSION_RPC_DEADLINE` plus 250 ms, and the TUI reducer under test produces its next frame; a site that hangs fails the test on a timeout rather than hanging CI. A nested lock (holding `SessionStore::lock` and calling `mutate_session_store`) returns an error within 1 s. Both are red with the deadline or the guard removed.

11. The programme row `docs/plans/2026-09-12-desktop-programme.md:126` is flipped to done with the PR numbers and the proof run id, in the implementation PR.

─ WHICH EXISTING TESTS MUST STAY GREEN ─

· `ainb-tui/crates/ainb-core/tests/session_cli_daemon.rs`, `ainb-hangar-daemon/tests/it_session_import.rs`, `ainb-hangar-store/tests/sessions.rs`, `migration_upgrade_full_chain.rs` and `tripwire_migrations_apply.rs`.
· The session-persistence tests that pin the file today: `ainb-core/tests/behavioral/session_persistence.rs`, `tests/orphan_session_removal.rs`, `tests/tripwire_plain_checkout_workspace_name.rs`, `tests/tripwire_stopped_session_keeps_its_label.rs`, `ainb-hangar-daemon/tests/tripwire_ccc_interactive_session_visible_to_fleet.rs` with `tests/tripwire_support/mod.rs:368`. Each keeps passing against the file path, or is converted to the daemon path in its own commit with the reason.
· `ainb-app/tests/host_side_effects.rs`, `serialize_guard.rs` with its fixture untouched, `state_serde.rs`, `bindings.rs`.
· The `session_manager.rs` pu4 concurrency tests (`:4817` onward on `v2`), converted to run against the resolver if the sites they pin move.
· CI: fmt, Contracts, Test (ubuntu, macos), ainb-core tripwires, hangar-e2e, web e2e, and `.github/workflows/desktop.yml` including the Desktop journey.

─ SCOPE, STAGED AS PRs ─

**P6e-1, reconciliation (daemon only, capability still dark).**
· Touches: `repo/sessions.rs`, `session_import.rs`, `lib.rs`'s boot call.
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

1. **Tombstones: decided, no.** While P6d is dark the only table deletes are the daemon's own retried-pane replacements (`shadow_write_session`), whose file entry is replaced under the same key, so there is nothing a reconcile could resurrect. A tombstone table would add an obligation at every delete site, forever, for an empty hazard set. Resurrection is prevented instead by keeping the file current, deletes included (see "Mixed versions").

2. **Does a session created after the flip survive a downgrade?** Decided: yes. The file stays written, row by row under its flock, with the table ("Mixed versions"). Writes from an older binary are picked up by the repeatable reconcile; deletes from an older binary are not, and that is stated and tested (criterion 7b). Removing the file write is a later node with its own spec note.

3. **Where does the reconciliation marker live?** Decided: a second row in `session_import` keyed `<path>#reconcile`, written by the first successful reconcile pass in the same transaction as its rows. No migration: widening the primary key would rebuild the table in SQLite and drag `migration_upgrade_full_chain`. `import_complete` becomes kind-aware: `SessionsRepo::any_import_completed` (P6d `repo/sessions.rs:299`, today `COUNT(*) > 0`) is replaced by a check that BOTH the `<path>` import row and the `<path>#reconcile` row exist for the file the daemon resolves, so the P6d row alone never reports the table authoritative.

4. **Do the reducer's synchronous reads block the TUI loop?** Decided: they are bounded, not measured. `state.rs` reads the store synchronously in reducers and effects, and `load_session_store` goes through `run_async`'s `block_in_place` on the TUI's multi-thread runtime. Every session RPC `SessionSource` makes carries a named deadline (`SESSION_RPC_DEADLINE`, recommended 2 s), and taking the `sessions.json` flock is bounded by the same deadline (a try-lock loop, not a blocking `flock`). On expiry the call returns an error that the reducer surfaces; it never hangs. Nesting is forbidden: no code holds `SessionStore::lock` or `lock_sessions_store*` while it calls `load_session_store` or `mutate_session_store`, because a second `flock` on a second descriptor in the same process blocks forever. `state.rs:13392-13395` (lock, then load) becomes one resolver call. A thread-local held-lock guard turns a nested attempt into an immediate error instead of a deadlock. Criterion 10.

5. **What counts as "every combination" for `p6-concurrent`?** Recommended: the four combinations `scripts/surface-combo-smoke.sh:10` already defines, each with a CLI leg added, rather than a fresh matrix; the scenario reuses that script's fail-closed setup.

6. **The snapshot path (`snapshot.rs:92-95`) copies `sessions.json`.** Decided: no interim mirror. The file stays current after the flip ("Mixed versions"), so the verbatim copy keeps working; moving snapshots onto the table is filed as #1213 for the snapshots-index move base spec `:286` names next.

─ FOLLOW-UPS TO FILE, NOT TO SOLVE ─

· Removing the downgrade file write (open question 2) once a release has shipped with the table authoritative.
· The thirteen fields spelled out in the repo row, the wire entry and the converters (P6d review, minor).
· Gating the sessions handlers on the capability server-side, together with the other `hangar.*` capabilities.
· Snapshots taking the table's rows instead of the file: #1213.

─ FINAL DELIVERABLE ─

Confirmation each criterion is satisfied. Every file created or modified. How to run, test and deploy. Proof (test output, the `p6-concurrent` result, the desktop journey run id). Decisions made and anything to know. Known limitations and follow-ups.

Begin by outputting your plan. Then execute end-to-end without checking in until done or genuinely blocked.

─ PROGRESS LOG ─

Plan, staged as the PRs above:
1. P6e-1: reconciliation and its marker, capability still dark.
2. P6e-2: every `ainb-app` reader and writer onto `SessionSource`, the call-site tripwire, then the flip as the last commit.
3. P6e-3: `p6-concurrent` and the programme row.
