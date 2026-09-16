# /goal P6 closes slice 2 at the daemon edge: `ainb-hangar-client` gains reconnect with 1s/4s/16s backoff and resync on hello, `ainb-web` throws away its own socket client and dials through that one client with its playwright journey gating the PR in CI, and `sessions.json` becomes a daemon table behind RPC with a one-time import, so the TUI, the web and the CLI run concurrently against one daemon as a scripted proof scenario and R1 can start from one client with one reconnect story

─ CONTEXT ─

· First act in the worktree: the checkout sits on `v2`; run `git switch -c p6-client-web` before any edit, then copy this file to `docs/plans/goals/2026-09-16-p6-client-web.md` and commit it as the first signed commit (`git -c gpg.format=openpgp -c user.signingkey=907EC78C72C6AFF6 commit -S`). Every later file change is its own signed commit; never `git add -A`. Push the branch and open each PR as a draft against `v2`, then report to the orchestrator by putting the PR number and head sha in the PR body's first line and leaving the terminal idle with that same line as your last message on screen; the orchestrator reads this terminal and brings the verdict.

· Project: agents-in-a-box (ainb) desktop programme, slice 2 node P6, the last node of slice 2.
  - Programme row: `docs/plans/2026-09-12-desktop-programme.md:126`, "P6 client reconnect, web onto client, sessions.json to daemon | 2 | planned | P5 | web e2e green; TUI + web + CLI concurrent smoke". Gate to start: P5, met (P5a #1043, P5b #1057, P5c #1061, P5d #1065, all on `v2`).
  - Base spec `docs/plans/2026-09-04-desktop-shared-core-spec.md`: the P6 row (`:113`), the crate table (`:70-71`), the local transport row (`:280`), the daemon singleton and durable-state notes (`:284-286`), the surface-hazard audit (`:310-319`), the errors table (`:334-343`), the testing strategy (`:350-356`), and the open question this node answers (`:384`).
  - Multi-surface spec `docs/plans/2026-09-11-multi-surface-decisions-spec.md`: the component rows for `ainb-hangar-client` (`:219`) and `ainb-web` (`:223`), the deferral row for the web renderer (`:284`), and R1's dependency on this node (`:18`, `:180`, `:293`).
  - Sibling goals whose format and voice this one matches: `docs/plans/goals/2026-09-14-p5-hosts-close-slice-2.md` and `docs/plans/goals/2026-09-15-d1-desktop-shell.md`.

· Stack:
  - Rust workspace under `ainb-tui/`. `ainb-web` is a member at `ainb-tui/Cargo.toml:8`, `ainb-hangar-client` at `:32`; `default-members` is `ainb-core` plus `ainb-hangar-daemon` (`:47`) and `crates/ainb-desktop` is the one excluded workspace (`:42`).
  - The daemon store is SQLite behind `sqlx` with 100 numbered migrations under `ainb-tui/crates/ainb-hangar-store/migrations/`, newest `0100_daemon_identity.sql`.
  - The web dashboard is axum with SSE plus one WebSocket terminal route, a vanilla-JS frontend under `ainb-tui/crates/ainb-web/frontend/`, and a playwright suite under `ainb-tui/crates/ainb-web/e2e/` pinned at `@playwright/test` 1.49.1.
  - CI is `.github/workflows/ci.yml` with the jobs `changes` (`:76`), `fmt` (`:116`), `cli-docs` (`:149`), `mirror-fanout` (`:221`), `bindings` (`:259`), `test` (`:300`), `contracts` (`:464`), `ainb-hooks` (`:520`), `fleet-send-tmux` (`:727`), `codex-launch-tmux` (`:803`), `core-tripwires` (`:876`), `hangar-e2e` (`:936`), `acp` (`:1097`), `recordings` (`:1122`), `chat-bus-smoke` (`:1158`) and `machete` (`:1187`), plus `.github/workflows/desktop.yml` and `.github/workflows/tripwire-exclusions.yml`.

· What is on `v2` that P6 changes, seam by seam. Every path and line below exists on `origin/v2` today.

  The one client, and what it does not have yet.
  - `ainb-tui/crates/ainb-hangar-client/src/lib.rs`, 1,351 lines. `DaemonClient` (`:218`) with `from_env` (`:353`), `with_parts` (`:369`), `set_surface` (`:378`) and `socket` (`:386`); `socket_path` (`:205`) and `socket_path_in` (`:212`); `daemon_host_id` (`:40`); `DaemonError` (`:126`) with its classifier (`:161`).
  - Four retained subscriptions live there: `ConnectionSubscription` (`:225`), `FleetSubscription` (`:264`) whose `next_event` (`:273`) yields `FleetStreamEvent::Revision` or `ResyncRequired` (`:286`), `MessageSubscription` (`:294`) and `TranscriptSubscription` (`:322`). `fleet_subscribe` (`:490`) and `open_fleet_subscription` (`:504`) both take `after_revision`, which is the replay primitive a resync needs and already works.
  - There is no reconnect in that file. `git grep` over the crate finds `backoff` only in `presence.rs`. Every ordinary call opens a fresh connection and every retained subscription ends at the first `DaemonError`; the caller is left holding the error. This is the gap the programme row names first.
  - `ainb-tui/crates/ainb-hangar-client/src/presence.rs`, 395 lines, is the only thing in the tree that already reconnects. Its header diagram is at `:8-13`; `Dialer` (`:89`), `PresenceLease::spawn` (`:108`) and `spawn_with` (`:114`), `PresenceState` (`:73`) published on a `watch` channel via `state()` (`:134`), `close` (`:139`), `mark_process_as_surface` (`:62`), `lease_held` (`:67`). Its loop doubles a backoff from `BACKOFF_INITIAL` to `BACKOFF_MAX`, 250 ms to 2 s (`:204`, `:221`, `:234`, `:236`), and resets on a successful hello. P6's reconnect is the same shape at the spec's cadence, and `PresenceState` is the precedent for the banner state a renderer reads.
  - The only production consumer of a retained fleet subscription is the terminal host's status task: `ainb-tui/crates/ainb-core/src/agent_status_host.rs:383` opens it, `:389` folds a revision, and `:398` handles `ResyncRequired` with an empty body. `ainb-tui/crates/ainb-app/src/fleet/control.rs:39` imports the same event type. Anything P6 adds to the client has exactly one existing caller to convert.
  - The client's dependencies are `ainb-hangar-proto`, `ainb-hangar-core`, `tokio`, `serde`, `serde_json`, `thiserror` and `tracing` (`ainb-tui/crates/ainb-hangar-client/Cargo.toml`). Nothing host-shaped, which is why `ainb-web` can depend on it with no cycle.

  The second socket client, which this node deletes.
  - `ainb-tui/crates/ainb-web/src/daemon.rs`, 742 lines, is a complete parallel implementation of the same transport on the same proto crate. Its own `DaemonError` (`:57`), `socket_path` (`:100`), `DaemonClient` (`:111`), `from_env` (`:121`), `with_parts` (`:131`), the `UnixStream::connect` (`:216`), the framing pair `write_frame` (`:373`) and `read_frame` (`:407`) with `read_response` (`:395`), `read_token` (`:366`), and `RPC_TIMEOUT` at 5 s (`:41`).
  - It carries its own presence too: `WebPresence` (`:257`) with its `Drop` (`:278`), `maintain_web_presence` (`:287`), `PresenceConnection` (`:327`), and its own constants `PRESENCE_HEARTBEAT` 60 s (`:46`), `PRESENCE_RETRY_INITIAL` 100 ms (`:48`) and `PRESENCE_RETRY_MAX` 5 s (`:51`). The client's `PresenceLease` pings at 30 s and backs off 250 ms to 2 s. Two surfaces, two cadences, one registry.
  - Only three RPCs ride it: `attention_list_fleet` (`:138`), `fleet_status` (`:157`) and `answer` (`:167`). All three already exist on the shared client at `lib.rs:392`, `:458` and `:421`.
  - What is NOT transport and must survive the deletion unchanged in behaviour: `display_kind` (`:443`), `attention_to_needs` (`:464`), `attention_to_needs_with_status` (`:479`), `stamp_status` (`:554`), `normalize_payload` (`:581`), and the `Answerer` trait (`:621`) with `DaemonAnswerer` (`:633`) that gives the route tests their seam.
  - Its consumers are few and named: `ainb-tui/crates/ainb-web/src/data.rs:292` builds a client per poll, `:322` projects the cards; `src/lib.rs:50` re-exports `Answerer`, `DaemonAnswerer`, `DaemonClient` and `DaemonError`, and `:89` spawns `WebPresence`; `src/routes.rs:33` imports the answerer, `:87` holds it as `Arc<dyn Answerer>`, `:111` picks the production one, `:293` mounts `POST /api/answer`. The WS terminal at `routes.rs:277` (`/ws/session/:id`, handler in `src/terminal.rs`) is not a daemon-client consumer and P6 does not touch it.
  - `ainb-tui/crates/ainb-web/Cargo.toml` depends on `ainb-hangar-proto`, `ainb-app` and `ainb-hangar-core`, and NOT on `ainb-hangar-client`. Adding that one dependency is the whole swap on the manifest side.
  - `ainb-tui/crates/ainb-web/tests/daemon_client.rs`, 171 lines, tests the transport being deleted. Its assertions about framing and hello belong to the client's own tests after the swap; its assertions about projection belong to whatever module keeps the projection.

  The web journey, which exists and does not run in CI.
  - `ainb-tui/crates/ainb-web/e2e/tests/ask-answer.spec.ts` is the real ask-answer journey: the daemon-seeded three-option ASK renders, option two is clicked, the answer routes through `POST /api/answer`, the daemon performs the verified tmux send, the card leaves the open inbox and the store row reads answered by web with pick 2.
  - `ainb-tui/crates/ainb-web/e2e/playwright.config.ts` refuses to run without `WEB_URL`, ignores the record specs by pattern, and runs one worker with a 60 s test timeout and a 15 s expect timeout. `playwright.config.record.ts` and `tests/ask-answer.record.spec.ts` are the screenshot pair and stay out of the gate.
  - The provisioner is `scripts/hangar/run_web_e2e.sh` at the repo root, not under `ainb-tui/`. `docs/hangar/verify-converged-goal.md:108` records the journey as CC18 and green; `:110` documents exactly what the script does and states in terms: "Not CI-gated on this branch". A `git grep` for playwright, e2e or ask-answer across `.github/workflows` returns nothing but comments. So the journey is real, passes locally, and guards nothing.
  - The script's stated requirements are `tmux`, `sqlite3`, `node` and `npm`, and it exits 2 naming the missing tool. It builds `ainb`, `ainb-hangar-daemon` and the `seed_control_center` example into the shared target, provisions a short `/tmp` HOME for the 104-character unix socket limit, seeds the ASK, starts `ainb web` with a bearer token, installs the playwright chromium on demand, and tears down by exact name and pid only.

  `sessions.json`, its two path resolvers, its four writers and its ten readers.
  - Two modules resolve the same file. `ainb-tui/crates/ainb-fleet-core/src/fleet/session_registry.rs:84` is `sessions_json_path()`, `$AINB_HOME` else the home dir else `.`, joined with `.agents-in-a-box/sessions.json` (`:89`). `ainb-tui/crates/ainb-app/src/interactive/session_manager.rs:1263-1269` is `SessionStore::storage_path()`, the same join from the same base. Its header at `session_registry.rs:13` says it plainly: "is the one `~/.agents-in-a-box/sessions.json` file, keyed by tmux session name".
  - The file-locked writer is `session_registry.rs`: `register_session` (`:100`) and `register_session_at` (`:116`), the flock on `sessions.json.lock` (`:137-146`), `lock_sessions_store_at` (`:168`) and `lock_sessions_store` (`:180`), and the atomic temp-plus-rename write (`:186-193`). The audit row at base spec `:318` names this exact file and line: "cross-process flock + temp + rename, best-effort ... Stevie's call: daemon owns it ... P6 moves it into the daemon behind RPC".
  - The other writer is `SessionStore` (`session_manager.rs:1143`) with `SessionStoreGuard` (`:1154`), `load` (`:1160`), `lock` (`:1209`) and the locked read-modify-write `mutate`. Its doc comment at `:1194-1196` states that it takes the SAME lock `ainb_fleet_core::session_registry::register_session_at` takes, which is what makes the two writers safe today and what the daemon must subsume rather than break.
  - The record shape is `SessionMetadata` (`session_manager.rs:94-124`), thirteen fields: `session_id`, `tmux_session_name`, `worktree_path`, `workspace_name`, `created_at`, `agent_type`, `headroom_enabled`, `rtk_enabled`, `skip_permissions`, `model`, `model_source`, `codex_model`, `codex_thread_id`. The daemon's own registration writes the five-field subset `AinbSessionRecord` (`session_registry.rs:42-56`) from `ainb-tui/crates/ainb-hangar-daemon/src/run_loop.rs:1807`, calling `register_session` at `:1812`. So the daemon is already a writer of this file, through the file, from inside the process that is meant to own it.
  - CLI readers and writers in `ainb-core`, the ones the criterion moves onto RPC: `cli/list.rs:107` (load), `cli/recover.rs:95` (load), `:111` (`storage_path` reported to the user), `:405` (mutate on resume), `:521` (lock) and `:524` (load under it), `cli/run.rs:318` (mutate on create), `cli/status.rs:138` and `:182` (mutate on removal), `cli/git_cmd.rs:110` and `:170` (load). The shared resolver both the CLI and the TUI call is `ainb-tui/crates/ainb-app/src/cli/util.rs:20`.
  - Reducer-side and service-side users in `ainb-app`, which are not in the criterion but are the blast radius: `app/events.rs`, `app/state.rs`, `app/snapshot.rs:92-95` (a snapshot copies the file verbatim), `components/session_recovery.rs`, `git/worktree_manager.rs`, `headroom/mod.rs`, `interactive/mod.rs`, `interactive/session_manager.rs`, `config/persist.rs`. Tests that pin the file: `ainb-core/tests/behavioral/session_persistence.rs`, `tests/orphan_session_removal.rs`, `tests/tripwire_plain_checkout_workspace_name.rs`, `tests/tripwire_stopped_session_keeps_its_label.rs`, and `ainb-hangar-daemon/tests/tripwire_support/mod.rs:368` with its caller `tests/tripwire_ccc_interactive_session_visible_to_fleet.rs:123`.

  The daemon side of the move.
  - Migrations are numbered SQL files under `ainb-tui/crates/ainb-hangar-store/migrations/`; there are 100 and the newest is `0100_daemon_identity.sql`, so this node's is `0101`.
  - `0100` is the precedent to copy for shape, not just for number. Its typed wrapper is `ainb-tui/crates/ainb-hangar-store/src/repo/daemon_identity.rs`: the module header names the migration and the spec decision (`:1`), the stateless wrapper sits at `:46`, the mint is an `INSERT OR IGNORE` (`:68`) whose zero-row case is an explicit error against the migration's CHECK (`:80`), and the read is one statement (`:146`). Its migration test is `ainb-tui/crates/ainb-hangar-store/tests/daemon_identity.rs`, and the chain test that every new migration must keep green is `tests/migration_upgrade_full_chain.rs` with `tests/tripwire_migrations_apply.rs`.
  - No existing table is the CLI session store. `fleet_session` (`migrations/0044_fleet_control_plane.sql:12`) is the status family D14 owns, `fleet_acp_session` (`0079_chat_bus.sql:67`) is the chat bus, `standup_session` (`0028_atc_standup.sql:115`) is unrelated. The crate table at base spec `:71` gives the daemon a "sessions table (moved from sessions.json)", which is a new table, not a column on `fleet_session`.
  - Hello already carries everything the migration needs to be safe across versions. `HelloParams` (`ainb-tui/crates/ainb-hangar-proto/src/auth.rs:58`) carries `protocol` (`:73`) and `capabilities` (`:80`); `HelloResult` (`:133`) carries the daemon's own with `has_capability` (`:163`); the daemon answers from `catalogue_strings()` at `ainb-tui/crates/ainb-hangar-daemon/src/rpc/auth.rs:401-404` and on the incompatible path at `:433-436`. The catalogue is the `CAP_*` block at `ainb-tui/crates/ainb-hangar-proto/src/protocol.rs:146-192`, `PROTOCOL_VERSION` is 1 (`:46`) and `negotiate` is `:123`. A new session-store capability string goes in that block and a client that does not see it advertised keeps reading the file.
  - `methods.rs:19` is `workspace/subscribe` and `:24` is `workspace/list`, which is the family base spec `:286` points at: "Surfaces read via RPC and `workspace/subscribe`".

  The proof harness this node extends.
  - `ainb-tui/scripts/proof/run.sh` with `lib.sh`, `summarize.py`, `README.md` and eighteen scenarios under `scenarios/`. `ALL_NODES` is the array at `:71-77`; `:80` fails the run if a named node has no scenario file, and `:116` sources it. A node not in that array does not run.
  - `ainb-tui/scripts/surface-combo-smoke.sh` is S-D's four-combination surface smoke, which fails closed and is already named in CI. It is the nearest prior art for the concurrent smoke and should be read before writing a second one.

· Locked decisions, as the specs state them. A node PR does not reopen one; a change needs a spec amendment PR first ("Rules of the road" in the programme doc).
  - One client. `ainb-hangar-client` is "the one daemon client" and owns "dial, hello, reconnect + resync, subscriptions, transcript stream" (base spec `:70`). Reconnect belongs there and nowhere else. The multi-surface component row (`:219`) counts the debt as "three hardcoded `UnixStream::connect` sites".
  - The daemon owns durable session state. "sessions table (moved from sessions.json)" (base spec `:71`); "Durable state: sessions.json, snapshots index, usage cache ownership moves into the daemon (converged invariant 1), sessions first. Surfaces read via RPC and `workspace/subscribe`" (`:286`); "P6 moves it into the daemon behind RPC" (`:318`).
  - The reconnect cadence is fixed. "client reconnect with backoff 1s/4s/16s, banner 'reconnecting', sections frozen with stale badge, resync on hello" (base spec `:255`). Those four are one behaviour, not four options.
  - The WS terminal's own recovery is separate and also fixed: "tab shows 'reconnecting' overlay, buffer kept" with "auto-redial 3x, then 'reattach' button" (base spec `:337`). D1c already implemented that half for the desktop; P6 does not change the web's terminal route.
  - `ainb-web` migrates its daemon client, not its renderer. "daemon client replaced by `ainb-hangar-client` in P6" (multi-surface `:223`); "keeps `PtyBridge` + WS framing (reused by desktop); migrates onto `ainb-hangar-client`" (base spec `:72`). Rewriting the web renderer onto the shared core is explicitly deferred to after M1 (multi-surface `:284`).
  - The concurrency gate is a PR gate from this node onward: "TUI + desktop + web started in every combination against one daemon; answer race, double spawn, shared file writes ... must pass on PR after P6" (base spec `:355`).
  - R1 starts from this node. Multi-surface `:18` ("R1 after P6"), `:180` (R1's gate reads "W0-wire, P6") and `:293` ("after W0-wire, P6, spikes 2 and 3"). R1 adds a WebSocket carrier and per-host reconnect with jittered backoff 1 s to 60 s on top of what P6 lands, so P6's reconnect is written to be re-parameterised, not rewritten.
  - The migration path is an open question this node closes, not invents: "sessions.json to daemon table: migration path for existing `~/.agents-in-a-box/sessions.json` and the CLI commands that read it" (base spec `:384`).

─ WHERE RECONNECT LIVES, AND WHAT A RENDERER READS ─

The spec puts reconnect in the client and a banner plus a stale badge in every renderer. Those are two seams, and only one of them is new code in a client.

```
┌──────────────┐   ┌──────────────┐   ┌──────────────┐
│ TUI          │   │ ainb-web     │   │ ainb-desktop │
│ agent_status │   │ data.rs poll │   │ D1 frame pump│
└──────┬───────┘   └──────┬───────┘   └──────┬───────┘
       │                  │                  │
       └──────────────────┴──────────────────┘
                          │  DaemonClient + watch<ConnState>
                          ▼
┌──────────────────────────────────────────────────────┐
│ ainb-hangar-client: dial · hello · 1s/4s/16s · resync│
└──────────────────────┬───────────────────────────────┘
                       │ hangar.sock
                       ▼
            ┌───────────────────────┐
            │ ainb-hangar-daemon    │
            └───────────────────────┘
```

· The backoff, the redial and the resync-on-hello are one loop in the client, built in the shape `presence.rs` already proves: a `watch` channel of connection state, a backoff that resets on a successful hello, and an abort on drop. `PresenceState` (`presence.rs:73`) is the model for the published enum and `PresenceLease::state` (`:134`) for how a renderer subscribes to it.
· Resync is not new protocol. `fleet_subscribe` and `open_fleet_subscription` already take `after_revision` (`lib.rs:490`, `:504`), and `FleetStreamEvent::ResyncRequired` (`:286`) already exists for the lag case. Reconnect means reopening the subscription at the last revision the caller folded, and the one existing consumer (`agent_status_host.rs:383-398`) is where that revision lives today.
· "Sections frozen with a stale badge" is renderer work over a connection state the client publishes. The TUI and the web read it directly. The desktop already has host-keyed staleness from D1b, so it needs the state, not a second mechanism.
· What P6 does NOT do here: it does not add a carrier, a transport enum variant, jitter, or a per-host reconnect. Those are R1's, and R1's row names them. P6 lands one local reconnect at the spec's cadence and leaves the timings behind named constants so R1 re-parameterises rather than rewrites.

─ THE ORDER THE THREE CRITERIA MUST LAND IN ─

The web swap and the web e2e gate are one criterion, and the safe order is the gate first.

```
P6a reconnect ──▶ P6b e2e in CI ──▶ P6c web onto client ──▶ P6d sessions to daemon
   (client)         (net first)        (under the net)         (+ concurrent smoke)
```

The journey at `ainb-tui/crates/ainb-web/e2e/tests/ask-answer.spec.ts` passes today against the transport P6c deletes. Landing it in CI before the swap makes the swap's PR show the journey green on the new client. Landing it after would mean the first CI run of the journey is also the first run of the new transport, with no baseline to blame. `docs/hangar/verify-converged-goal.md:110` already records the journey as green locally, so the gate PR is CI plumbing over a known-passing suite.

─ WHAT P6 MUST NOT DO ─

· No second reconnect implementation. When P6a lands, `ainb-web/src/daemon.rs`'s `maintain_web_presence` (`:287`) and its three retry constants (`:48`, `:51`, `:46`) are deleted, not retuned. One client, one cadence.
· No rewrite of the `ainb-web` renderer, its frontend, or its SSE shape. Multi-surface `:284` puts that after M1. `frontend/app.js` changes only if the swap changes a payload, and then the e2e says so.
· No change to the WS terminal route. `routes.rs:277` and `src/terminal.rs` keep their `PtyBridge`, which base spec `:72` says the desktop reuses. The terminal's own reconnect rule (`:337`) is not the daemon client's.
· No `Serialize` on a section and no serialisation call site outside `wire/`. `ainb-tui/crates/ainb-app/tests/serialize_guard.rs` with `tests/fixtures/serialize_call_sites.txt` stays green untouched.
· No new wire field without the key-path fixture and the bindings regenerated in the same PR: `ainb-tui/crates/ainb-app/tests/fixtures/section_key_paths.txt`, `tests/state_serde.rs`, and `bindings/AppState.ts`.
· No edits to `ainb-tui/crates/ainb-core/src/app/*`, the standing lane rule.
· No silent schema fork. The daemon's own `register_session` call at `run_loop.rs:1812` moves onto the table in the same PR as the table, or the daemon writes the file and the table and the goal log says which is authoritative and why.
· No destructive migration. The import reads the existing `~/.agents-in-a-box/sessions.json` once, leaves the file on disk, and is idempotent. A user who downgrades still has their sessions.
· No hand-run gate. The concurrency smoke is a scripted scenario under `ainb-tui/scripts/proof/scenarios/` registered in `run.sh`'s `ALL_NODES`, and the web e2e is a CI job, not a README instruction.
· No polling of CI from inside the lane. The orchestrator brings the verdict.

─ SUCCESS CRITERIA (ALL MUST BE TRUE) ─

1. `ainb-web` dials the one client and its journey gates the PR. `ainb-tui/crates/ainb-web/Cargo.toml` depends on `ainb-hangar-client`; `ainb-tui/crates/ainb-web/src/daemon.rs` contains no transport, meaning its `UnixStream::connect` (`:216`), `write_frame` (`:373`), `read_frame` (`:407`), `read_response` (`:395`), `read_token` (`:366`), `socket_path` (`:100`), its `DaemonClient` (`:111`) and its `DaemonError` (`:57`), and the whole `WebPresence` family (`:257`, `:278`, `:287`, `:327`) with `PRESENCE_HEARTBEAT`, `PRESENCE_RETRY_INITIAL` and `PRESENCE_RETRY_MAX` (`:46`, `:48`, `:51`) are all gone, replaced by `ainb_hangar_client::DaemonClient` and `PresenceLease` with `SurfaceKind::Web`; the projection half (`display_kind` `:443`, `attention_to_needs` `:464`, `attention_to_needs_with_status` `:479`, `stamp_status` `:554`, `normalize_payload` `:581`) and the `Answerer` seam (`:621`, `:633`) keep their behaviour and their tests; `src/data.rs:292`, `src/lib.rs:50` and `:89`, and `src/routes.rs:33` are updated with no behaviour change visible to a browser; and the playwright journey `ainb-tui/crates/ainb-web/e2e/tests/ask-answer.spec.ts` runs in a named CI job on the PR and is green on `ubuntu-latest`, provisioned by `scripts/hangar/run_web_e2e.sh` with `tmux`, `sqlite3`, `node` and `npm` installed on the runner, the record specs still excluded by `playwright.config.ts`, the chromium install cached or pinned, and the job name and run id recorded on the PR. `hangar/connections_list` shows exactly one `web` row while `ainb web` runs and none after it exits, asserted in a test, so the registry does not regress when the presence cadence changes from 60 s to 30 s.

2. The client reconnects at 1s/4s/16s and resyncs on hello, proved by killing a real daemon mid-subscription. `ainb-hangar-client` publishes a connection state on a `watch` channel in the shape `PresenceState` (`presence.rs:73`) set, redials with the spec's 1 s, 4 s then 16 s backoff (base spec `:255`) from named constants, resets the backoff on a successful `auth/hello`, and reopens each retained subscription at the caller's last folded revision through the existing `after_revision` parameter (`lib.rs:490`, `:504`). `ainb-tui/crates/ainb-core/src/agent_status_host.rs:383-398`, the one production consumer, is converted to it and no longer ends its task on a `DaemonError`. A test against a real `ainb-hangar-daemon` in a private hangar home opens a fleet subscription, `SIGKILL`s the daemon mid-stream, and asserts in one run: the published state goes to reconnecting with the observed delays matching 1 s, 4 s and 16 s within tolerance; a renderer reading that state shows the reconnecting banner and marks its sections stale and frozen rather than empty; after the daemon restarts, hello succeeds, the backoff resets, and the subscription resumes from the last revision with the events since it replayed contiguous to head and no gap. A second test covers the socket vanishing with no daemon coming back: the state stays reconnecting, the badge stays, and nothing panics or spins.

3. `sessions.json` is a daemon table behind RPC, imported once, with the CLI reading through the daemon and the three surfaces proved concurrent. Migration `0101` adds the sessions table in the shape of `0100_daemon_identity.sql`, with a typed repo wrapper in the shape of `src/repo/daemon_identity.rs` and its own migration test beside `tests/daemon_identity.rs`, and `tests/migration_upgrade_full_chain.rs` plus `tests/tripwire_migrations_apply.rs` stay green. The table carries the thirteen fields of `SessionMetadata` (`session_manager.rs:94-124`) so nothing is dropped, and a new RPC pair in the `workspace` family (`methods.rs:19`, `:24`) reads and mutates it behind a capability string added to `protocol.rs:146-192` and advertised through `catalogue_strings()` (`rpc/auth.rs:404`). A one-time import reads an existing `~/.agents-in-a-box/sessions.json`, is idempotent across restarts, leaves the file in place, and is covered by three tests: a fresh home imports nothing, a populated file imports every record once, and a second boot imports nothing further. The daemon's own registration at `run_loop.rs:1812` writes the table. The ten CLI sites (`cli/list.rs:107`, `cli/recover.rs:95`, `:405`, `:521`, `:524`, `cli/run.rs:318`, `cli/status.rs:138`, `:182`, `cli/git_cmd.rs:110`, `:170`, through the shared resolver at `ainb-app/src/cli/util.rs:20`) read and write through the daemon when it advertises the capability, and fall back to the file when it does not, so `ainb list` works with no daemon running. The flock pair in `ainb-fleet-core/src/fleet/session_registry.rs:137-193` and `SessionStore::lock` (`session_manager.rs:1209`) stay correct for the fallback path and are not deleted in this node. And a new proof scenario `ainb-tui/scripts/proof/scenarios/p6-concurrent.sh`, registered in `run.sh`'s `ALL_NODES` (`:71-77`), starts a TUI, an `ainb web` and a CLI invocation against one daemon in every combination, creates a session from one surface and asserts the other two see it, answers from one and asserts the other two fold it, and writes a `result.json` with `pass: true`; `bash ainb-tui/scripts/proof/run.sh --only p6-concurrent` passes on a `v2` build and a full harness run still reports every other node passing.

4. Final deliverable runs without errors

5. You can show proof (screenshot · test output · URL)

─ WHICH EXISTING TESTS MUST STAY GREEN ─

Unchanged on every PR of this node, or changed only in their own commit with the reason in the message:
· `ainb-tui/crates/ainb-app/tests/host_side_effects.rs`: `REACHABLE_TODAY`, `CALL_SITES`, `REDUCER_DISK_WRITES`, `HOST_STATE_READS`, the host tmux lookup fence and the `HostOnlyState` not-`Serialize` probe. A session read that becomes an RPC must not make a host crate reachable from `ainb-app`.
· `ainb-tui/crates/ainb-app/tests/serialize_guard.rs` with `tests/fixtures/serialize_call_sites.txt`.
· `ainb-tui/crates/ainb-app/tests/state_serde.rs` with `tests/fixtures/section_key_paths.txt`, regenerated with `UPDATE_SECTION_KEY_PATHS=1` in the same PR if a wire field moves, after triage against the deny-lists.
· `ainb-tui/crates/ainb-app/tests/bindings.rs` and the CI job "TypeScript bindings freshness", with `bindings/AppState.ts` regenerated and committed in the same PR if it moves.
· `ainb-tui/crates/ainb-hangar-store/tests/migration_upgrade_full_chain.rs` and `tests/tripwire_migrations_apply.rs`, on the `0101` PR above all.
· The session-persistence tests that pin the file today: `ainb-tui/crates/ainb-core/tests/behavioral/session_persistence.rs`, `tests/orphan_session_removal.rs`, `tests/tripwire_plain_checkout_workspace_name.rs`, `tests/tripwire_stopped_session_keeps_its_label.rs`, and `ainb-hangar-daemon/tests/tripwire_ccc_interactive_session_visible_to_fleet.rs` with `tests/tripwire_support/mod.rs:368`. Each either keeps passing against the fallback path or is converted in its own commit with the reason.
· `ainb-tui/crates/ainb-web/tests/routes.rs`, `tests/depth.rs` and `tests/session_source.rs`. `tests/daemon_client.rs` is the one test file this node is allowed to shrink, because it tests the transport being deleted; what it asserts about projection moves with the projection.
· `ainb-tui/crates/ainb-app/tests/command_gate.rs`, `parity.rs`, `renderer_free.rs`, `terminal_host_contract.rs`, `plugin_actions.rs`, `persistence.rs`, `effects.rs`, `intent_dispatch.rs`, `key_only_commands.rs`.
· CI jobs "fmt", "cli-docs", "Mirror fan-out bench", "TypeScript bindings freshness", "Contracts", "Test (ubuntu-latest)", "Test (macos-latest)", "ainb-core tripwires (ubuntu-latest)", "hangar-e2e", "machete", the excluded-set tripwire workflow, and `.github/workflows/desktop.yml`.
· `ainb-tui/scripts/surface-combo-smoke.sh`, S-D's existing four-combination smoke, which the new scenario complements rather than replaces.

─ SCOPE, STAGED AS PRs ─

Four PRs. Each is mergeable alone, targets `v2`, and carries its own proof.

**P6a, reconnect and resync in the one client.**
· Touches: `ainb-tui/crates/ainb-hangar-client/src/lib.rs`, a new module beside `presence.rs` for the reconnect loop, and `ainb-tui/crates/ainb-core/src/agent_status_host.rs` as the one consumer.
· Seams consumed: `presence.rs`'s `watch` channel and backoff shape, `fleet_subscribe` and `open_fleet_subscription` with `after_revision`, `FleetStreamEvent::ResyncRequired`, `DaemonError`'s classifier at `lib.rs:161`.
· Builds: a published connection state, a redial loop at 1 s, 4 s, 16 s from named constants, a hello that resets the backoff, and a resubscribe at the caller's last folded revision. The caller keeps owning the revision; the client owns the socket.
· Proof: a test against a real `ainb-hangar-daemon` in a private hangar home that `SIGKILL`s it mid-subscription and asserts the delays, the state transitions, the resumed revision and the contiguous replay; a second test for a daemon that never returns; and a unit test that the published state drives a frozen-with-stale-badge render rather than an empty one.
· Gate: `cargo test -p ainb-hangar-client -p ainb-core`, `cargo clippy --workspace -- -D warnings`, `cargo fmt --all -- --check`.

**P6b, the web journey in CI.**
· Touches: `.github/workflows/ci.yml` (a new job), `scripts/hangar/run_web_e2e.sh` where the runner needs to be CI-safe, `ainb-tui/crates/ainb-web/e2e/package.json` for a committed lockfile, and `docs/hangar/verify-converged-goal.md:110` where the "Not CI-gated" line stops being true.
· No Rust behaviour changes in this PR. It is the net, and the net goes up before the swap.
· Builds: one job on `ubuntu-latest` that installs `tmux`, `sqlite3` and Node 22, installs with `npm ci` from a committed lockfile, pins or caches the playwright chromium, runs the runner script, and fails closed on a missing tool rather than skipping. The record configs stay excluded by `playwright.config.ts`'s `testIgnore`.
· Proof: the job green on this PR against the transport still in place, with its name and run id on the PR body, and a deliberate red run recorded in the goal log to show the gate actually fails when the journey fails.
· Gate: the new job green, every existing job unchanged, `actionlint` clean.

**P6c, `ainb-web` onto `ainb-hangar-client`.**
· Touches: `ainb-tui/crates/ainb-web/Cargo.toml`, `src/daemon.rs`, `src/data.rs`, `src/lib.rs`, `src/routes.rs`, `tests/daemon_client.rs`.
· Seams consumed: `ainb_hangar_client::{DaemonClient, DaemonError, PresenceLease, mark_process_as_surface}`, `SurfaceKind::Web`, and the three RPCs the web actually calls, which the shared client already has at `lib.rs:392`, `:421` and `:458`.
· Deletes: the whole transport and presence half of `src/daemon.rs` named in criterion 1. What is left is projection and the `Answerer` seam, and the module is renamed or re-homed if that is what it now honestly is.
· Proof: the P6b job green on the new transport; a test that exactly one `web` row appears in `hangar/connections_list` while the server runs and none after; the route tests unchanged through the `Answerer` seam; and `cargo machete` clean, since `ainb-hangar-proto` may no longer be a direct dependency.
· Gate: `cargo test -p ainb-web`, the web e2e job, clippy and fmt.

**P6d, `sessions.json` into the daemon, and the concurrent smoke.**
· Touches: `ainb-tui/crates/ainb-hangar-store/migrations/0101_*.sql` with a new `src/repo/` module and its test, `ainb-tui/crates/ainb-hangar-proto/src/{methods.rs,protocol.rs}`, the daemon's RPC dispatch and `src/run_loop.rs:1807-1812`, `ainb-tui/crates/ainb-hangar-client/src/lib.rs` for the new calls, `ainb-tui/crates/ainb-app/src/cli/util.rs` and the ten `ainb-core/src/cli/*` sites, `ainb-tui/scripts/proof/scenarios/p6-concurrent.sh` with `run.sh`, and `docs/plans/2026-09-12-desktop-programme.md:126`.
· Seams consumed: `0100_daemon_identity.sql` and `repo/daemon_identity.rs` as the migration and wrapper precedent, the hello capability catalogue for the version gate, `session_registry.rs`'s flock for the fallback path, and `scripts/surface-combo-smoke.sh` as prior art for the scenario.
· Builds: the table, the typed wrapper, the RPC pair behind a capability, the idempotent one-time import, the CLI read path with a no-daemon fallback, the daemon's own write onto the table, and the concurrent scenario.
· Proof: the three import tests; a test that a CLI read works with the daemon stopped; a test that a session created by the daemon and one created by `ainb run` both appear once in `ainb list`; the `p6-concurrent` scenario's `result.json` with `pass: true`; a full proof run with every node passing; and the programme row flipped to done with the run ids in the same PR.
· Gate: `cargo test -p ainb-hangar-store -p ainb-hangar-daemon -p ainb-hangar-client -p ainb-core -p ainb-app --features test-support`, the proof scenario, clippy and fmt, and `hangar-e2e` on both runners.

─ CONSTRAINTS ─

· PRs target `v2`, never `main`, staged as the four above, each reviewable alone.
· Every file change is its own commit, GPG-signed with `git -c gpg.format=openpgp -c user.signingkey=907EC78C72C6AFF6 commit -S`. Never `git add -A` and never `git add .`; stage by named path.
· No attribution trailers, no `Co-Authored-By`, and no mention of Claude, an AI or any assistance anywhere in a commit message or a PR body.
· No em dashes on any line you author, in code, docs, commit messages or PR bodies.
· Never name the reference product this programme drew prior art from, in code, comments, docs, commits or PR bodies.
· Open every PR as a draft and flip it ready only when its own gate is green in CI. Never merge your own PR.
· Never poll CI. Message the orchestrator with the head sha when a PR is ready; the orchestrator brings the verdict.
· Locked decisions are not reopened. A change to one needs a spec amendment PR first, merged before the node PR that depends on it.
· Never touch `ainb-tui/crates/ainb-core/src/app/*`. Merge `origin/v2` before touching a file another lane is on, and name shared files in the PR body.
· The concurrency smoke is a scripted scenario under `ainb-tui/scripts/proof/scenarios/` registered in `run.sh`'s `ALL_NODES`, never a hand test and never a README instruction. The web e2e runs in CI on the PR, not locally only.
· Never kill a tmux server and never bulk-kill. Kill only by exact session name, `tmux kill-session -t <exact-name>`, and kill processes by port, never by process name.
· The lane runs on `claude-gcp` under `~/orca/workspaces/agents-in-a-box/p6-client-web`. Keep the disk under 85 percent: run `cargo build` only when a gate needs it, prefer `cargo test -p <crate>` over a workspace build, and `cargo clean` before a full build if the ceiling is near.
· `cargo clippy --workspace -- -D warnings` and `cargo fmt --all -- --check` in the pre-push gate on every commit.
· Every frontend dependency is in a committed lockfile and CI installs with `npm ci`, so the build reaches the network for nothing the lockfile does not name.
· Record every decision in this goal file's progress log as you take it, in the voice the sibling goals use.

─ OPERATING RULES, NON-NEGOTIABLE ─

1. PLAN FIRST. Output a numbered task list before writing any code.
2. WORK AUTONOMOUSLY. Don't ask clarifying Qs unless genuinely blocked.
3. SELF-VERIFY. After every step: run tests, inspect output, confirm it worked.
4. DEBUG YOURSELF. If it fails, diagnose and fix. Don't hand it back.
5. USE EVERY TOOL. MCPs · terminal · web · code exec · pull real data.
6. NO PLACEHOLDERS. No TODOs · no stubs · real components and real states.
7. PROGRESS LOG. Track completed · in-flight · decisions · blockers.
8. STAY ON GOAL. Discoveries off-spec? Note and keep moving.
9. IF BLOCKED. Log the wall · continue everything parallelizable.
10. CHECK SUCCESS BEFORE STOPPING. Re-read criteria · confirm each is met.

─ QUALITY BAR ─

· Code: clean, typed, follows project conventions
· Design: looks like a well-funded startup shipped it
· Output: survives a senior code review
· Docs: every new pattern, env var and decision logged

─ FOLLOW-UPS TO FILE, NOT TO SOLVE ─

File each as an issue with its evidence. Do not fix it in this node.
· The snapshot path copies `sessions.json` verbatim (`ainb-tui/crates/ainb-app/src/app/snapshot.rs:92-95`). Once the daemon owns the table, a snapshot needs the table's rows, not the file, and that is a snapshots-index move base spec `:286` names as a later step ("sessions first").
· The remaining `ainb-app` readers of `SessionStore` outside the CLI: `app/events.rs`, `app/state.rs`, `components/session_recovery.rs`, `git/worktree_manager.rs`, `headroom/mod.rs`, `interactive/mod.rs`, `config/persist.rs`. The criterion moves the CLI commands; the reducer-side reads keep the fallback path and their own move is its own node.
· The usage cache, the third item in base spec `:286`'s durable-state list, untouched here.
· `ainb-tui/crates/ainb-web/src/terminal.rs`'s `PtyBridge` at 80x24 against the tmux sizing rule at multi-surface `:232`. Not a P6 change, but the concurrent smoke may be the first thing that shows it.
· Whether `ainb-hangar-client` should own the retained-subscription revision instead of the caller. P6 leaves it with the caller because `agent_status_host.rs` already tracks it; R1's per-host reconnect may want it inside.
· The jitter and the 1 s to 60 s ceiling R1's row names (multi-surface `:180`). P6's constants are written to be re-parameterised, and the issue records which constants.
· `ainb-tui/crates/ainb-web/tests/daemon_client.rs`'s framing assertions, if they turn out to cover a case `ainb-hangar-client`'s own tests do not. File the gap against the client rather than keeping a duplicate transport test alive.

─ OPEN QUESTIONS THE LANE ANSWERS ON A PR BODY ─

· Whether the reconnect loop is a wrapper type around `DaemonClient` or a mode on it. Recommended: a wrapper, because `DaemonClient`'s ordinary calls are deliberately stateless with a fresh connection each (`lib.rs:1-15` header) and only the retained subscriptions need the loop. Answer on P6a.
· Where the new sessions RPC pair sits in the method vocabulary. Recommended: the `workspace` family, because base spec `:286` points surfaces at "RPC and `workspace/subscribe`" and `methods.rs:19` already has it. Answer on P6d with the method names.
· Whether the CLI fallback is per call or decided once at startup from the advertised capability. Recommended: once per process from `HelloResult::has_capability` (`auth.rs:163`), the way the section-20 host task picks its read path, so one `ainb list` cannot straddle two sources. Answer on P6d.
· Whether the import runs in the daemon at boot or on the first session RPC. Recommended: at boot, next to the migration, so a surface never races it. Answer on P6d.
· What the concurrent scenario counts as "every combination". Recommended: the four `scripts/surface-combo-smoke.sh` already runs, extended with the CLI leg, rather than a fresh matrix. Answer on P6d.
· Whether `ainb-web/src/daemon.rs` keeps its name after the transport leaves it. It would then hold only projection and the `Answerer` seam. Answer on P6c.

─ FINAL DELIVERABLE ─

Confirmation each criterion is satisfied. Every file created or modified. How to run, test and deploy. Proof (screenshot, test output, URL). Decisions made and anything to know. Known limitations and follow-ups.

Begin by outputting your plan. Then execute end-to-end without checking in until done or genuinely blocked.

─ PROGRESS LOG ─

Plan, staged as the four PRs:
1. P6a: reconnect and resync in `ainb-hangar-client`, with `agent_status_host.rs` converted and the daemon-kill test. (Complete, PR #1166)
2. P6b: the web playwright journey in CI as its own job, the net before the swap. (Complete)
3. P6c: `ainb-web` onto `ainb-hangar-client`, its own transport and presence deleted, projection kept.
4. P6d: migration `0101` and the sessions table behind RPC, the one-time import, the CLI read path with a fallback, the `p6-concurrent` proof scenario, the programme row.

- 2026-09-16 P6a: Reconnect loop in ainb-hangar-client with 1s/4s/16s backoff, watch-channel ConnectionState, hello resync via after_revision, agent_status_host converted, SIGKILL integration test green. PR #1166 opened against v2.
- 2026-09-16 P6b: Added web-e2e Playwright journey job to .github/workflows/ci.yml. Committed package-lock.json for ainb-web/e2e and updated .gitignore. Updated run_web_e2e.sh to default CARGO_TARGET_DIR safely and use npm ci. Updated verify-converged-goal.md. Deliberate red run verified: mismatched assertion failed closed with Playwright exit code 1; green run passed with exit code 0.
