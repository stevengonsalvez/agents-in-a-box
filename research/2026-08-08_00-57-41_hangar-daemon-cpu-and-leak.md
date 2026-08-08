# Research: Hangar daemon CPU burn and memory/disk growth — cause and fix

**Date**: 2026-08-08 00:57:41
**Repository**: agents-in-a-box
**Branch**: f/cpu-fix-hangar-daemon
**Commit**: bbee2dc3
**Research Type**: Codebase + live-system forensics

## Research Question

"There is a problem with hangar, can you investigate. There are times it leaks and hogs out CPU."

A prior session diagnosed this as an fsevents-triggered rescan storm in
`scan_all_summaries`, and reported a migration-76 lockout of the released
binary as a secondary find.

## Executive Summary

The prior diagnosis was **half right and wrong about the trigger**. There is no
fsevents watcher on provider transcripts in the daemon at all — the daemon polls
`events.jsonl` on a 3-second timer. And the usage scanner is only the *second*
biggest burner.

Four independent defects are running concurrently, a fifth multiplies all of
them, and there is no memory leak in the classic sense at all:

1. **`tmux_missing` write-amplification loop** — the *disk* leak and a large share
   of CPU. A 3-second reconciler emits a fresh, never-deduplicated `fleet_event`
   per stuck session per tick, forever. It has produced **832,711 rows / 76% of an
   847 MB table**, grown `hangar.db` to **1.6 GB**, and made the reconciler's own
   query take >1 s — which, with tokio's default `Burst` missed-tick behaviour,
   collapses the 3 s timer into a continuous hot loop.
2. **Cache-less full-corpus usage rescan** — re-parses ~5.7 GB of provider JSONL
   from cold on every refresh, re-triggered per hook line with no debounce, so it
   runs effectively back-to-back. Introduced **the day before this investigation**
   (2026-08-07).
2b. **Whole-transcript slurp on the hook path** — the sharpest *RSS* driver.
   `read_lines` materialises an entire transcript into a `Vec<String>` to read the
   last 320 rows, twice per classify, on live files of **137 MB / 103 MB / 89 MB**.
3. **Infinite Codex transport retry**, backoff capped at 16 s: ~3,670 failed
   child-process spawns per day, forever, each able to leak a grandchild.
4. **No daemon singleton** — not weak enforcement, *none*. Four daemons are alive
   right now against one `hangar.db`, so 1-3 all run ~4× concurrently and contend
   on one SQLite file.

There is **no leaking container** anywhere in the daemon: broadcast channels are
capped, the cancel registry is guard-deregistered, ACP pool maps all have removal
paths, and there are no zombie processes. The RSS curve is transient allocation
churn from 2 and 2b, not retention.

Root causes are all in `crates/ainb-hangar-daemon/` and `crates/ainb-fleet-core/`.
None is fixed on any branch; `f/cpu-fix-hangar-daemon` is currently bit-identical
to `origin/main`.

## Key Findings

- The `tmux_missing` loop is the disk leak. 93% of the event log is tmux
  reconciler churn that should never have been written.
- "Leak" and "CPU hog" have **different** causes. The growth is on disk
  (defect 1 + defect 5); the RSS is transient churn (defects 2 and 2b); the CPU
  is all of them plus a runaway `Burst` ticker.
- The usage rescan is a one-day-old regression, and the incremental cache it
  should use **already exists in the same crate** and is already used correctly
  by the session-reader plugin.
- The migration-76 story is real but is a **symptom of defect 4**: dev builds
  share the production hangar home because nothing stops them, and the released
  binary is four migrations behind, not one.
- The daemon has no fs watcher over `~/.claude/projects`. The recursive watcher
  the prior session blamed lives in the TUI process and is correctly throttled
  (3 s debounce + 300 s floor).
- Several defects carry a code comment describing the exact failure they still
  have (`fleet.rs:1240`, `lib.rs:670-680`, `rpc/mod.rs:213-217`). The knowledge
  was there; the guard was not.

## Prior Learnings

| Learning | Key insight | Confidence |
|---|---|---|
| `reference_never_cp_over_running_binary` | Install to a fresh name, then `<newbin> hangar daemon restart` | high |
| `feedback_all_daemons_in_daemons_screen` | Every ainb daemon must show in the `d` overlay — relevant, four invisible daemons are running | high |
| `reference_event_loop_decouple_poll_from_tick` | Split poll cadence from work cadence — exactly the fix shape for defect 1 | high |
| `feedback_verify_dont_assert` | Evidence, not assertion — applied throughout below | high |

## Detailed Findings

### Defect 1 — `tmux_missing` write-amplification loop (CONFIRMED)

**Severity: highest.** This is both the "leak" and a large share of the CPU.

```
┌──────────────────────────────┐  interval(3s), MissedTickBehavior = Burst (default)
│ spawn_tmux_reconciler        │
│ fleet.rs:1256-1279           │
└──────────────┬───────────────┘
               ▼
      reconcile_tmux_once()  ── tmux discovery + per-session get_session
               ▼
      FleetRepo::snapshot(pool)      SESSION_SELECT_ALL, 1472 rows, 1.086 s
               ▼
      for row in snapshot.sessions:
          skip if tmux_target.is_none()
               or discovered.contains(key)
               or (lifecycle_state=='EXITED' AND transport_health=='UNAVAILABLE')
               ▼  (guard never satisfied for MANAGED sessions — see below)
          INSERT fleet_event  event_id = "tmux:missing:{key}:{observed_at}"
               ▼                                          └─ timestamp ⇒ never a dup
          emit_fleet_revision(rev)
               ▼
      broadcast::Sender<i64>, capacity 256  (events.rs:43,87)
               ▼
      spawn_fleet_forwarder rx.recv()       (rpc/mod.rs:659)
               ├─ Ok  ─▶ events_after_wire(cursor, 1024) — paged, revision-indexed read
               └─ Lagged ─▶ send "fleet/resync_required" then **return** (forwarder dies)
                              ▼  client re-subscribes
                        subscription_projection()  — 1472-row SELECT
                        + one extra fleet_event lookup per session with a
                          current_request_fingerprint, in one transaction,
                          against the 847 MB / 1.1M-row table
```

**Correction to an earlier reading:** a revision bump does *not* itself drive
`subscription_projection`. The forwarder does a paged, indexed
`WHERE revision > ? LIMIT 1024` read (`repo/fleet.rs:687-691`), and
`subscription_projection` has only two call sites, both request-driven
(`rpc/mod.rs:1219` `fleet/snapshot`, `rpc/mod.rs:1261` `fleet/subscribe`).

The amplification is real but indirect: at 2.3 revisions/second against a
256-slot broadcast, any subscriber stalled ~110 s hits `RecvError::Lagged`, and
`rpc/mod.rs:661-668` responds by sending `fleet/resync_required` and **`return`ing
— killing its own forwarder task**. The client re-subscribes, which *is* the
1472-row SELECT plus a genuine N+1 (`repo/fleet.rs:628-651`). So the event rate
drives the re-subscribes that drive the expensive query.

**The bug**, `crates/ainb-hangar-daemon/src/fleet.rs:1194-1211` (the skip guard) vs
`fleet.rs:1218-1235` (the event it emits):

```rust
// fleet.rs:1218  tmux_missing_event()
patch: FleetSessionPatch {
    capabilities: Some(capabilities_for_tmux_state(row, false)),
    lifecycle_state: (row.management_state != "MANAGED"
        && row.lifecycle_authority == "inferred")
        .then(|| "EXITED".to_string()),      // ← None for every MANAGED session
    transport_health: Some("UNAVAILABLE".to_string()),
    ..FleetSessionPatch::default()
},
```

The skip guard needs **both** `lifecycle_state == "EXITED"` and
`transport_health == "UNAVAILABLE"`. The event always sets the second and
conditionally sets the first. For any session that is `MANAGED` or whose
lifecycle is `authoritative` (i.e. every hook-backed session), the first is never
set, so the session can never reach the skip condition and re-emits on every tick
for the rest of the daemon's life. The timestamped `event_id` defeats the
`result.duplicate` short-circuit.

**Fixed 2026-08-08.** The guard now keys off `transport_health` alone — the field
`tmux_missing_event` actually always writes — so the emit is a transition, not a
state re-assertion. Self-healing: when tmux reappears the discovery loop puts the
session in `discovered` and flips `transport_health` back to `HEALTHY`, so the
next disappearance is a real transition and emits again. The condition is
extracted as `needs_tmux_missing_event(row, discovered)` so it is testable
without a live tmux server. `MissedTickBehavior::Delay` set on the 3 s ticker.

Two tests cover it, and both were mutation-checked: restoring the original
`lifecycle_state == "EXITED" && …` conjunct turns them red
(`"tick 0: re-emitting is the 832k-row bug"`), and the fix turns them green. 419
daemon lib tests pass; clippy at exact parity on the file (18 before, 18 after).

The author fixed exactly this class of bug in the *discovery* path directly
above, and documented it — `fleet.rs:1240-1242`:

> `// a permanent mismatch and append one no-op `fleet_event` per discovery`
> `// tick, forever, for every hook-backed session.`

`tmux_row_matches` gained a `lifecycle_settled` term for that. The exit-reconcile
path below it never got the equivalent guard.

**Live evidence** (`~/.agents-in-a-box/hangar.db`):

| Measure | Value |
|---|---|
| `fleet_event` rows | 1,097,679 |
| `fleet_event` size (dbstat) | 847 MB |
| `fleet_provider_event` size | 624 MB |
| `hangar.db` total | 1.6 GB (+ 1.1 GB `hangar.db.pre-78.bak`) |
| `tmux_missing` rows | 832,711 (76% of all events) |
| + `tmux_available` / `tmux_unavailable` | 1,025,124 combined = **93.4%** |
| distinct sessions stuck in the loop | 1,178 (31 matching right now) |
| worst single session | `claude:unrelated` — 174,235 events |
| rate on 2026-08-07 | 197,406 `tmux_missing`/day = 2.3/s sustained |

Growth curve — this is recent and accelerating:

| Date | fleet_event rows written |
|---|---|
| 2026-07-29 | 64 |
| 2026-07-30 | 348 |
| 2026-07-31 | 4,754 |
| 2026-08-01 | 15,588 |
| 2026-08-03 | 97,235 |
| 2026-08-05 | 221,727 |
| 2026-08-07 | **313,823** |

The stuck sessions match the predicted shape exactly:

```
session_key                                   lifecycle_state  lifecycle_authority  management_state  transport_health
claude:unrelated                              TURN_COMPLETE    authoritative        MANAGED           UNAVAILABLE
claude:618a359c-1d0e-4a1a-b5c1-766a3f0ec5d0   STARTING         authoritative        MANAGED           UNAVAILABLE
claude:4016db87-e81b-4a33-8ef4-192a5c295b38   TURN_COMPLETE    authoritative        MANAGED           UNAVAILABLE
```

`lifecycle_authority = authoritative` and `management_state = MANAGED` — the
precise combination that makes the patch's `.then()` return `None`.

**Why it becomes >100% CPU**, not just disk growth: `fleet.rs:1263` is

```rust
let mut ticker = tokio::time::interval(std::time::Duration::from_secs(3));
```

with no `set_missed_tick_behavior`, so tokio's default `Burst` applies. Only
`acp_pool.rs:732,1390` set `Delay` anywhere in the daemon. The per-iteration work
already exceeds 3 s — the live log carries 44 `slow statement` warnings on this
exact query, `elapsed: 1.085596834s`, `rows_returned: 1396` — so the ticker fires
back-to-back with no sleep. It is a positive feedback loop: more rows → slower
query → more catch-up ticks → more rows.

**Contributing cause — dead sessions are never removed from the visible set:**

```
lifecycle_state  count (visible = 1)
EXITED           1440
TURN_COMPLETE      19
RUNNING             6
```

1,440 of 1,472 visible sessions are dead but still scanned by every
`SESSION_SELECT_ALL`.

### Defect 2 — full-corpus usage rescan with no cache (CONFIRMED)

**Severity: high.** This is the 123% CPU spike and the RSS 469 → 1151 MB.

`crates/ainb-hangar-daemon/src/fleet_usage.rs:243-272` `scan_all_summaries()`
calls `scanner::scan_since(&roots, since)` with `since` = 30 days ago
(`fleet_usage.rs:257`).

`scan_since` (`crates/ainb-plugin-session-reader/src/scanner.rs:382-391`) opens
with `let mut cache = None;`. With `ctx.cache == None`, `read_one_cached`
(`parsers/mod.rs:112-172`) skips the SQLite lookup entirely (line 122) and always
does `std::fs::read_to_string(path)` (line 139) + a full re-parse (line 152).

There *is* an mtime prune (`parsers/mod.rs:80-84`) that skips files whose whole
mtime predates the cutoff — but an actively appended session refreshes its mtime
on every write, so the prune is a no-op for exactly the ~30 live sessions that
matter. Every one is opened and fully re-parsed on every refresh.

Corpus actually re-parsed per refresh, measured on this host:

| Source | Size | Files |
|---|---|---|
| `~/.claude/projects` (mtime within 30 d) | 2.7 GB | 4,174 |
| `~/.codex/sessions` | 3.0 GB | 702 |

Plus, per file parsed, an unconditional `std::thread::sleep(500us)` duty-cycle
throttle (`parsers/mod.rs:159`) — intended to soften a cold scan, but on this
path *every* scan is cold.

**Amplification passes** inside `fleet_usage.rs`, per refresh:
- `usage.calls.iter().filter(..).cloned().collect()` — clone #1 per call (line 280-285)
- `scanner::aggregate(calls.clone())` — clone #2, whole Vec (line 286)
- `groups.entry(..).push(call.clone())` — clone #3 (line 402)
- × 3 periods (`Today`, `Trailing7Days`, `Trailing30Days`, lines 260-270)
- up to 19 total fold/sort/bucket passes over overlapping subsets: 1 in
  `scan_since` + 3 × (1 + up to 5 provider groups)

`ProviderCall` carries 6 `String` + 2 `Vec<String>` fields including
`user_message`, so these are deep clones of heap-heavy structs, not memcpy.

**Trigger — no debounce.** The prior session's fsevents theory is wrong; the real
trigger is `crates/ainb-hangar-daemon/src/attention_ingest.rs:335`, which calls
`fleet_usage::request_refresh()` (and `fleet_quota::request_refresh()` at :336)
**once per hook line**, inside the per-line loop of `ingest_once`
(`attention_ingest.rs:207-227`), on a 3-second poll of `events.jsonl`
(`attention_ingest.rs:67` `TICK = 3s`). `request_refresh` coalesces *concurrent*
calls via the `state.refreshing` flag (`fleet_usage.rs:114-122`) but does not
rate-limit *sequential* ones — the moment a scan finishes, the next queued hook
line starts another.

Measured hook rate on this host: **2,566 events/hour peak** (0.71/s), every one
carrying a `session_id`, so every one fires the call. A cold scan of 5.7 GB takes
far longer than the inter-arrival gap ⇒ the daemon is scanning continuously.
That is a sufficient, fully cited mechanism for one saturated core.

**On the "leak" framing:** the persisted state (`cached: Mutex<Option<CachedSummaries>>`,
`fleet_usage.rs:137`) is wholesale-replaced each refresh and is bounded by
`FLEET_USAGE_MAX_DAILY_BUCKETS` / `FLEET_USAGE_MAX_BREAKDOWN_BUCKETS`. Nothing
accumulates raw calls across refreshes. Nor is there a leaking container anywhere
else in the daemon — broadcast channels are capped at 256 (`events.rs:43`), the
cancel registry is guard-deregistered on every exit path including panic-unwind
(`cancel.rs:63-72`), and every `acp_pool` map has a removal path (`:783`, `:2406`).
The only never-evicting map is `PR_STATUS_CACHE` (`pr_status.rs:289`, inserted at
`:333`, never removed), bounded by distinct PR URLs and therefore small.

So the RSS 469 → 1151 MB is **not** classic unbounded retention. It is transient
multi-hundred-MB allocation churn that the allocator never returns — driven by
defect 2 above and, more sharply, by defect 2b below.

The genuine unbounded growth is on disk (defect 1, plus the artefacts in defect 5).

### Defect 2b — whole-transcript slurp on the hook path (CONFIRMED)

**Severity: high — this is the sharpest RSS driver.**

`crates/ainb-fleet-core/src/fleet/read/jsonl_tail.rs:458-462`:

```rust
fn read_lines(path: &Path) -> Option<Vec<String>> {
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    Some(reader.lines().map_while(Result::ok).collect())   // ENTIRE file
}
```

The whole transcript is materialised into a `Vec<String>` in order to use only
the newest ≤320 rows — `last_api_error_from_jsonl` immediately does
`lines.len().saturating_sub(window)` (`:474-479`). `last_ask_user_question`
(`:291`) does the same, and `needs.rs:148-165` calls **both in sequence**, so a
session with no open ask pays two full reads.

Live transcripts on this host, all modified today:

```
137 MB  .../cochilli-feat-shopify/c1f17e6f-....jsonl
103 MB  .../stevengonsalvez-github-io/92e0458a-....jsonl
 89 MB  .../cochilli--feat-pdp-imagery/53869b87-....jsonl
```

A 137 MB file becomes a `Vec<String>` of several hundred MB once per-`String`
capacity and pointer overhead are counted — twice per classify. Call sites:
`attention_ingest.rs:346` (per hook line; `is_qualifying` at `:74-79` admits both
`Stop` and `SubagentStop`), `standup.rs:594`, `atc.rs:527`.

This, not allocator drift alone, is the most plausible mechanical account of a
1.1 GB RSS on a daemon whose baseline is ~470 MB.

#### Fixed 2026-08-08, with a measured A/B

`read_lines` replaced by `read_tail_lines(path, max_rows)`: seek to
`min(len, MAX_TAIL_BYTES)` before EOF, read forward, discard the leading fragment
(a backward seek can land mid-row), cap at `MAX_TAIL_ROWS = 320` — the deepest
window any caller in the module asks for. All four call sites converted, including
`last_narrative_snapshot`, which had its own inline copy of the slurp.

Like-for-like benchmark, same 89 MB / 5,863-row live transcript, same operation
(20 × `last_ask_user_question` + `last_assistant_info`), fix applied vs
`git checkout`'d baseline:

| | time | held in memory |
|---|---|---|
| baseline | 2,659 ms | 89 MB `Vec<String>` |
| bounded tail | **1,139 ms** | ≤ 4 MiB |

**2.3× faster, 57% less time, ~22× less resident.**

*An earlier measurement in this session compared the old slurp against the new
slurp plus full ask-detection and appeared to show no win. That comparison was
invalid — different amounts of work on each side. The numbers above are the
corrected like-for-like A/B.*

Five tests added, and the window cap was mutation-checked: deleting the `drain`
turns two of them red, so the bound is load-bearing rather than decorative.

#### Residual found by that benchmark — ALSO fixed

57 ms per classify probe remained after the bounded tail, and it was not I/O.
`last_ask_user_question` walks an exponential window (20 → 40 → 80 → 160 → 320)
and re-decoded the same rows at every step; within each step
`find_open_ask_user_question` decoded every row once in `resolved_tool_use_ids`
and again in its reverse walk — roughly **1,240 JSON decodes to examine 320
rows**. At ~15 KB per row that is megabytes of redundant `serde_json` per call,
and it is what the `sample(1)` profile's `skip_to_escape` dominance was actually
showing. The slurp was the memory story; this was the CPU story.

Fixed by decoding once: `parse_rows()` returns `Vec<Option<Value>>` (a non-JSON
row stays in place as `None`, so window arithmetic still counts rows the way the
transcript does), and `resolved_tool_use_ids`, `find_open_ask_user_question` and
`synthesize_from_rows` now take pre-decoded rows. `last_narrative_snapshot` had
the identical window/re-decode shape and was converted too.

#### Combined result

Same 89 MB / 5,863-row live transcript, same operation, three states:

| state | time (20 iterations) | vs baseline |
|---|---|---|
| baseline (`main`) | 2,659 ms | — |
| + bounded tail | 1,139 ms | 2.3× |
| + decode-once | **443 ms** | **6.0×** |

Plus ~22× less resident memory (89 MB `Vec<String>` → ≤ 4 MiB).

Verification: 97 crate tests green, `rustfmt` clean, downstream
`ainb-hangar-daemon` and `ainb` compile, and clippy is at **exact parity with
baseline** (32 warnings before, 32 after — the three the change initially added
were fixed rather than tolerated).

**This is a one-day-old regression.** `fleet_usage.rs` history:

| Commit | Date | Subject |
|---|---|---|
| `37a03e29` | 2026-08-07 | feat(hangar): summarize provider usage |
| `60b933bb` | 2026-08-07 | perf(fleet): bound usage history scans |
| `9ba50b8c` | 2026-08-07 | feat(fleet): cache usage summaries |

### Defect 3 — infinite Codex transport retry (CONFIRMED)

`crates/ainb-hangar-daemon/src/fleet_provider/codex_manager.rs:302-319`, backoff
at `:384`:

```rust
fn service_backoff(attempt: usize) -> Duration {
    Duration::from_secs(1_u64 << attempt.min(4))   // caps at 16s, retries forever
}
```

With `startup_timeout: Duration::from_secs(5)` (`:65`), the effective cycle is
~21 s, forever, with no give-up and no circuit breaker.

Live log, `~/.agents-in-a-box/hangar/logs/daemon.2026-08-07`:

```
{"level":"WARN","fields":{"message":"Codex managed transport unavailable",
 "attempt":171,"error":"provider transport failed: Codex initialize timed out"},
 "target":"ainb_hangar_daemon::fleet_provider::codex_manager"}
```

3,670 occurrences in that one day — ~165/hour, flat across all 24 hours. Each
attempt spawns a codex app-server child that then times out; the log also carries
`reaped orphaned codex app-server processes (periodic)`, so the children are
being cleaned up after the fact rather than not spawned.

### Defect 4 — no daemon singleton (CONFIRMED)

Four daemons are running right now against the same `~/.agents-in-a-box` home:

```
pid    uptime      binary
64839  2h37m       .../worktrees/by-name/agents-in-a-box--fix-fleet-send-integrity--effd752b/.../debug/ainb
80101  8d16h22m    ~/.claude/jobs/cc83c6a4/tmp/fix-reap/wt/.../debug/ainb
 5702  8d16h18m    ~/.claude/jobs/cc83c6a4/tmp/fix-reap/wt/.../debug/ainb
30185  8d15h51m    ~/.claude/jobs/cc83c6a4/tmp/fix-lockscope/wt/.../debug/ainb
```

Three are 8+ days old and were launched from throwaway agent-job worktrees that
no longer represent anything. Only one can hold `hangar.sock`, but **all four run
the tmux reconciler, the usage poller, the attention ingest and the codex retry
loop**, and all four write to the one 1.6 GB SQLite file. Every defect above is
multiplied roughly 4×, plus SQLite lock contention which slows each iteration,
which feeds the `Burst` catch-up loop in defect 1.

The daemon also restarted **23 times on 2026-08-07** (`grep -c "hangar rpc
listening"`), up from 1-7 on prior days.

**The mechanism: there is no singleton enforcement anywhere.** Two launch paths,
only one of which checks anything:

| Path | Pre-check? |
|---|---|
| `ainb hangar daemon start` → `start_daemon_if_stopped()` (`ainb-core/src/cli/hangar/mod.rs:8439-8511`) | Yes — reads the pidfile, `pid_is_running()` (`mod.rs:7885-7889`, a `kill(pid,0)` probe), spawns only if dead. TOCTOU, plain `std::fs::write` (`mod.rs:8496`), no flock. |
| `ainb hangar daemon run` → `boot()` (`ainb-hangar-daemon/src/lib.rs:390-692`) | **None.** Unconditional new daemon. |

In `boot()` the only pidfile touch is `PidFile::register(&dir)` at `lib.rs:681` —
*after* store-open, migrations, seed, `fleet_usage`/`fleet_quota` install and the
RPC socket bind (`lib.rs:552`). It is a **write, not a guard**: nothing reads it
first, nothing aborts if a live daemon already owns the home.

The comment immediately above it (`lib.rs:670-680`) describes this exact failure
mode and claims to have closed it:

> `// was invisible: the TUI spawned a SECOND daemon,` `rpc::bind` `unlinked the`
> `// live socket out from under the first, and two claim loops + two sweepers`
> `// then raced one SQLite home while the TUI talked to the newcomer's empty`
> `// in-memory state. Writing the pid here closes that hole at the source.`

It does not. Writing the pid makes a rogue daemon *discoverable* to path 1's
check; it never stops path 2 from creating one, and nothing ever signals an
existing daemon to exit when a new one boots. `rpc::bind` (`rpc/mod.rs:210-222`)
then unconditionally `remove_file`s any existing socket, on the stated
assumption — `rpc/mod.rs:213-217`, *"this is safe because only one daemon owns a
given hangar home at a time"* — that is precisely the invariant nothing enforces.
Each rebind steals the socket; all four daemons keep their claim loops, sweepers,
reconcilers and usage pollers running against the DB regardless of who currently
holds it.

**Why dev builds land on the production home:** `AINB_HANGAR_HOME` is read
verbatim in `ainb_hangar_core::paths::hangar_home()`
(`crates/ainb-hangar-core/src/paths.rs:19,26-37`; consumed at
`ainb-hangar-store/src/store.rs:99-100`). `just dev` (`justfile:30-38`) exports
only `AINB_BIN`, never `AINB_HANGAR_HOME`, so every worktree defaults to
`~/.agents-in-a-box`. Nothing in `CLAUDE.md` or `docs/` tells a developer to set
it. (`just dev` does call `daemon restart`, so it is not the path that leaked the
three 8-day-old daemons — those came from direct `hangar daemon run` invocations
in job worktrees.)

**This is also the true cause of the migration lockout** the prior session
reported. The error is `sqlx`'s own `MigrateError::VersionMissing`, propagated by
a bare `?` in `apply_migrations()` (`ainb-hangar-store/src/lib.rs:118-121`) →
`Store::open_in` (`store.rs:92`) → rendered as `"database: unreachable: {e}"` in
`run_daemon_status()` (`ainb-core/src/cli/hangar/mod.rs:8371-8374`). Fail-closed
by design; nothing in this repo catches or overrides it. This branch carries 79
migrations, matching the DB: `0076_column_stage_prompt.sql` (Aug 4),
`0077_task_queue_agent_status_index.sql`, `0078_task_board_column.sql`,
`0079_chat_bus.sql` (Aug 6). So the released 1.18.0 is **four migrations behind,
not one**.

### Defect 5 — unbounded on-disk artefacts (CONFIRMED)

| Artefact | Size | Count | Prune? |
|---|---|---|---|
| `fleet_event` table | 847 MB | 1,097,679 rows | **none** — no `DELETE FROM fleet_event` anywhere outside tests |
| `fleet_provider_event` table | 624 MB | 53,423 rows | operator-triggered only (`delete_acp_before`, `repo/fleet_provider_event.rs:399-412`), never automatic |
| `~/.agents-in-a-box/hangar/provider-events/` | 738 MB | 54,778 files in one flat dir | none (`attention_ingest.rs:458`) |
| `~/.agents-in-a-box/events.jsonl` | 88 MB | 55,878 lines | none |
| `hangar.db.pre-78.bak` | 1.1 GB | — | manual |

### Defect 6 — secondary burners (CONFIRMED unless marked)

| # | Finding | Citation |
|---|---|---|
| 6a | `TmuxRun::wait` polls `tmux has-session` every **200 ms** for the whole run life (`max_runtime` 9,000 s ⇒ ~45,000 fork+execs per run) | `interactive.rs:53` `POLL_INTERVAL`, `:186-206`; impl is `Command::new("tmux").args(["has-session",…]).status()` in `ainb-fleet-core` `fleet/send/tmux.rs` |
| 6b | Auto-standup pays its full cost while disabled: `probe_idle_fleet(now)` (full fleet discovery + per-session `classify`) runs at `standup.rs:352`, but the `cfg.enabled` gate is at `:377`, **inside** `tick()`, after the work | `standup.rs:342-355,377` |
| 6c | Codex server spawn omits `.process_group(0)`, so `stop_child`'s SIGKILL misses grandchildren. `codex_orphans_to_reap` matches only `ppid == 1 && is_codex_app_server(args)` (`:828-845`), so a reparented `node` helper never matches. 3,670 failed attempts/day each get one chance to leak one. Observed on this host: pid 68494 `codex` → child 70105 `…/cua_node/bin/node_repl` | `codex_manager.rs:439-448` vs the explicit `.process_group(0)` + rationale at `runner.rs:1423-1432` |
| 6d | `attention_ingest` resets its byte cursor to 0 if `events.jsonl` ever shrinks below it, replaying all 88 MB at `MAX_INGEST_BYTES` 4 MB/tick — a ~66 s storm running `apply_hook` + `request_refresh` + a full-transcript `classify` per `Stop` line | `attention_ingest.rs:64,176-178,186` |
| 6e | `run_loop.rs:618` floors `dispatched_interval` at **1 ms**, so `HANGAR_SWEEP_INTERVAL_MS=1` yields a 1 ms DB sweep loop. Not the default — check the daemon's env before dismissing | `run_loop.rs:618` |
| 6f | `rpc/mod.rs:11374` allocates the declared Content-Length with no cap (`vec![0u8; len]`). Same-uid + token gated, so local-only, but a buggy client is an instant RSS spike | `rpc/mod.rs:11374` |
| 6g | *Hypothesis*: `beads_sync::sync_loop` may be dead in production — no `run_inbound_loop` call site found in `lib.rs`. Needs a 30-second confirm | `beads_sync/sync_loop.rs:38-64` |
| 6h | `event_log` has no retention; the outbox is total over `HangarEvent` including `TaskProgress`/`TaskMessage` heartbeats. Only deleted on workspace delete | `event_outbox.rs:56-100`, `workspace.rs:462-464` |

**Explicitly cleared** — do not spend time here: `sweeper.rs` (60 s / 30 s / 3600 s,
`:83-98`), `inbox_sweep.rs` (`KEEP_LAST = 200`, `:45`), `acp_pool.rs` (15 s sweeper
with `MissedTickBehavior::Delay` already set at `:732`), the `run_loop` claim loop
(1 s, `join_next` correctly gated on `!runs.is_empty()` at `:547`, no `None`-spin),
`beads_sync` backoff (exponential to a 5-min ceiling). **No zombie processes**:
`runner.rs:1425-1438` uses `process_group(0)` + `kill_on_drop(true)` + `killpg`;
`codex_manager.rs:1430-1435` `stop_child` does `start_kill` then `wait().await` on
every error branch; every `tmux`/`ps`/`lsof` call uses `.status()`/`.output()`,
both of which reap. The grandchild gap in 6c is the one real exception.

### What is NOT the cause

The prior session's fsevents-storm theory does not survive contact with the code.
Every `notify`/fsevents watcher in the workspace:

| File:line | Process | Watches | Throttle |
|---|---|---|---|
| `ainb-hangar-daemon/src/profile.rs:232-295` | **daemon** | the profiles dir (unrelated), non-recursive | `mpsc::channel(1)` + drain-loop coalescing |
| `ainb-core/src/models/usage_dir_watcher.rs:99-142` | **TUI only** | `~/.claude/projects`, `~/.codex/sessions`, gemini, copilot, cursor — recursive | 3 s debounce + **300 s floor** (`:49,:56`) |
| `ainb-fleet-core/src/fleet/read/jsonl_tail.rs:150-183` | CLI only | one transcript file, one-shot | n/a |

The daemon has **no** watcher over provider transcript directories. Its usage
refresh is driven by a 3-second poll of `events.jsonl`, not by fsevents.

## Code References

- `crates/ainb-hangar-daemon/src/fleet.rs:1194-1211` — exit-reconcile loop, missing skip guard **(defect 1)**
- `crates/ainb-hangar-daemon/src/fleet.rs:1218-1235` — `tmux_missing_event`, conditional `lifecycle_state` patch + timestamped `event_id`
- `crates/ainb-hangar-daemon/src/fleet.rs:1263` — `interval(3s)` with default `Burst` missed-tick behaviour
- `crates/ainb-hangar-daemon/src/fleet.rs:1240-1242` — the author's own comment describing this bug on the sibling path
- `crates/ainb-hangar-store/src/repo/fleet.rs:845-852` — `SESSION_SELECT_ALL`, the >1 s query
- `crates/ainb-hangar-store/src/repo/fleet.rs:616-640` — `subscription_projection`, 1472-row SELECT + N+1 over `fleet_event`
- `crates/ainb-hangar-daemon/src/fleet_usage.rs:243-272` — `scan_all_summaries` **(defect 2)**
- `crates/ainb-hangar-daemon/src/fleet_usage.rs:257` — the cache-less `scan_since` call
- `crates/ainb-hangar-daemon/src/fleet_usage.rs:280-286,402` — the three per-call clones
- `crates/ainb-plugin-session-reader/src/scanner.rs:382-391` — `scan_since`, `let mut cache = None;`
- `crates/ainb-plugin-session-reader/src/scanner.rs:541-634` — `scan_incremental`, **the fix that already exists**
- `crates/ainb-plugin-session-reader/src/plugin.rs:93-152` — `run_blocking_scan`, how the plugin uses it correctly
- `crates/ainb-plugin-session-reader/src/parsers/mod.rs:80-84,112-172,159` — mtime prune, cache bypass, 500 µs sleep
- `crates/ainb-hangar-daemon/src/attention_ingest.rs:67,207-227,335-336` — 3 s tick, per-line unthrottled `request_refresh` **(defect 2 trigger)**
- `crates/ainb-hangar-daemon/src/fleet_provider/codex_manager.rs:302-319,384` — infinite retry **(defect 3)**
- `crates/ainb-hangar-store/src/repo/fleet_provider_event.rs:399-412` — the only automatic-ish prune in the store
- `crates/ainb-hangar-daemon/src/lib.rs:670-681` — `PidFile::register`, a write not a guard **(defect 4)**
- `crates/ainb-hangar-daemon/src/rpc/mod.rs:210-222` — socket rebind on an unenforced "only one daemon" assumption
- `crates/ainb-core/src/cli/hangar/mod.rs:8384-8411` — `daemon run`, no pre-check
- `crates/ainb-core/src/cli/hangar/mod.rs:8439-8511,7885-7889` — `daemon start`, the one path that does check (TOCTOU)
- `crates/ainb-hangar-core/src/paths.rs:19,26-37` — `AINB_HANGAR_HOME` resolution
- `justfile:30-38` — `just dev`, exports `AINB_BIN` only
- `crates/ainb-hangar-store/src/lib.rs:118-121` — `MigrateError::VersionMissing` propagated bare
- `crates/ainb-fleet-core/src/fleet/read/jsonl_tail.rs:458-462` — `read_lines`, whole-file slurp **(defect 2b)**
- `crates/ainb-fleet-core/src/fleet/read/jsonl_tail.rs:291,474-479` — the two callers that each pay a full read
- `crates/ainb-hangar-daemon/src/rpc/mod.rs:659-668` — forwarder suicides on `Lagged`
- `crates/ainb-hangar-store/src/repo/fleet.rs:687-691` — the paged, indexed read the forwarder actually does
- `crates/ainb-hangar-daemon/src/interactive.rs:53,186-206` — 200 ms tmux fork poll **(defect 6a)**
- `crates/ainb-hangar-daemon/src/standup.rs:342-355,377` — work before the enabled gate **(defect 6b)**
- `crates/ainb-hangar-daemon/src/fleet_provider/codex_manager.rs:439-448,828-845` — missing `process_group(0)` **(defect 6c)**
- `crates/ainb-hangar-daemon/src/attention_ingest.rs:64,176-178` — cursor reset replay storm **(defect 6d)**

## Recommendations

> **Ranking superseded by measurement.** After the live recovery below, a
> `sample(1)` profile put defect 2b first by a wide margin. Do **10 → 3 → 1+2 →
> 5** in that order. The original list is kept as-is for its detail.

Ranked by CPU-and-growth relief per line of diff.

1. **Fix the `tmux_missing` skip guard** (`fleet.rs:1194-1211`). Emit only on a
   *state transition*. Either mirror the discovery path's `lifecycle_settled`
   idea — skip when `transport_health` is already `UNAVAILABLE` regardless of
   `lifecycle_state` — or drop the timestamp from `event_id` so the existing
   `result.duplicate` short-circuit does its job. This alone stops 93% of event
   writes and the DB growth.
2. **Set `MissedTickBehavior::Delay` on the reconciler** (`fleet.rs:1263`), and on
   the other daemon `interval()` sites that lack it (`attention_ingest.rs:527`,
   `standup.rs:344`, `fleet_quota.rs:157`, `run_loop.rs:624,651,753,812`,
   `fleet_usage.rs:148`). Turns a runaway catch-up burst into a bounded tick.
3. **Point `scan_all_summaries` at `scan_incremental` + `UsageCache`** rather than
   `scan_since`, exactly as `plugin.rs::run_blocking_scan` already does. The
   incremental machinery, the watermark partition and the unchanged-snapshot
   short-circuit all already exist in the same crate and were built for issue
   #255. This is a wiring change, not new architecture.
4. **Debounce `attention_ingest.rs:335-336`** — call `request_refresh` at most once
   per ingest tick, not once per line, and give the service a minimum interval
   between *sequential* refreshes, not just concurrent ones.
5. **Make the daemon a singleton.** Add a real pre-boot guard to `boot()`
   (`lib.rs:390-692`) — not just `PidFile::register` at `:681`, which only writes.
   Back it with `flock`/`O_EXCL` on the hangar home rather than the current plain
   `std::fs::write` (`lib.rs:356`, `mod.rs:8496`), so path 1's TOCTOU window
   closes too. Then `rpc::bind`'s stated assumption (`rpc/mod.rs:213-217`) becomes
   true instead of aspirational. Four concurrent daemons multiply every defect
   above and contend on one SQLite file.
6. **Give dev builds their own hangar home by default** — have `just dev`
   (`justfile:30-38`) export `AINB_HANGAR_HOME` derived per worktree unless a
   flag opts into the shared production home. This is what schema-locked brew
   1.18.0 out of the production DB; migration head is 79, so the released binary
   is four behind.
7. **Cap the Codex transport retry** (`codex_manager.rs:302-319`) — give up after N
   attempts, or open a circuit breaker, and surface it as a health warning
   instead of retrying forever.
8. **Add retention** — a periodic prune for `fleet_event`, `provider-events/` and
   `events.jsonl`. Even with defect 1 fixed, nothing bounds these.
9. **Retire EXITED sessions from the visible set** — 1,440 of 1,472 visible rows
   are dead, and every `SESSION_SELECT_ALL` pays for them.

Cheap wins worth taking in the same pass, roughly one line each:

10. **Replace `read_lines` with a bounded reverse tail** (last ~2 MB) and share one
    read between the ask-scan and the error-scan (`jsonl_tail.rs:458-462`).
    Biggest RSS win per line of diff in the whole list.
11. **Move the `cfg.enabled` check above `probe_idle_fleet`** (`standup.rs:352` vs
    `:377`). A default-off feature currently pays full price every 60 s.
12. **`interactive.rs:53`** — raise `POLL_INTERVAL` to ~2 s, or watch the exit file
    instead of forking `tmux` five times a second per run.
13. **Add `.process_group(0)` to the codex server spawn** (`codex_manager.rs:439-448`),
    matching `runner.rs:1423`.
14. **Don't reset the ingest cursor to 0** on a shrunk `events.jsonl`
    (`attention_ingest.rs:176-178`) — treat it as a rotation and start at EOF.
15. **Raise the `dispatched_interval` floor** off 1 ms (`run_loop.rs:618`) and cap
    the RPC body allocation (`rpc/mod.rs:11374`).

### Recovery for the current host — PERFORMED 2026-08-08 ~01:20-01:40

Executed, with measured before/after. **Read this section before acting on the
ranking above: it changes it.**

| Step | Result |
|---|---|
| Stopped all 5 live daemons (SIGTERM by exact pid) | all exited |
| Fresh consistent backup `hangar.db.bak-pre-tmuxprune-20260808-011847` | 1.7 GB, `integrity_check` = ok |
| Deleted tmux churn below `head - 10000` | **1,025,162 rows** removed, 83,327 kept |
| `VACUUM` | db **1.7 GB → 1.4 GB** |
| `integrity_check` / `foreign_key_check` after | ok / clean |
| Pruned `provider-events` older than 3 days | 59,408 → 46,135 files, 770 MB → 559 MB |
| Restarted daemon (auto, via `ensure_hangar_daemon`) | exactly one, `status` healthy, migrations applied |

**The query win is real and large.** The statement the daemon logged 44 times at
`elapsed: 1.085596834s`:

```
before (from daemon log):  1.086 s
after  (sqlite3, timed):   0.010 s      ~100x
```

Index bloat collapsed with it: `sqlite_autoindex_fleet_event_1` 94 MB → 6 MB,
`idx_fleet_event_session_revision` 66 MB → 6 MB.

**Two corrections this exercise forced:**

1. **tmux churn was 93% of ROWS but only ~15% of BYTES.** After the prune,
   `fleet_event` still holds 724 MB in 87k rows. The byte bulk was always
   payload-heavy hook events, which also have no retention:

   | event_type | rows | payload MB | max row |
   |---|---|---|---|
   | PostToolUse | 17,195 | 255 | 3.6 MB |
   | UserPromptSubmit | 909 | 246 | 2.3 MB |
   | PostToolBatch | 10,941 | 96 | 0.7 MB |
   | PreToolUse | 17,881 | 34 | 97 KB |

   So defect 1 is a *row-count* and *index* problem (hence the query latency and
   the `Burst` feedback loop), not the disk-space story. Recommendation 8
   (retention) matters more than first stated, and must cover hook payloads.

2. **The prune did NOT fix CPU.** A single freshly-started daemon on the pruned
   DB sits at **99.3% CPU / 537 MB RSS after 2m53s**. Whatever the slow query was
   costing, it was not the dominant burner.

`sample(1)` on that daemon resolves it — the hot blocking-pool stack is:

```
tokio blocking pool
  └─ attention_ingest::process_line
       └─ fleet::read::needs::classify
            └─ jsonl_tail::last_ask_user_question
                 └─ find_open_ask_user_question
```

and the top-of-stack weight is overwhelmingly JSON string scanning —
`serde_json::read::SliceRead::skip_to_escape` (1,031 samples, the single largest
`ainb` symbol), `parse_str_bytes`, `parse_escape`, `next_or_eof`, plus slice/chunk
work consistent with hashing.

**This makes defect 2b, not defect 1, the primary CPU cause.** Revised order for
the code work: **2b → 2 → 1 → 4**.

*Caveat on the absolute number*: this is a `target/debug` binary — the sample is
thick with `precondition_check` bounds-check frames that a release build elides.
A release build will be materially faster in absolute terms. The algorithmic
defects (O(file) per hook line, O(corpus) per refresh) are unaffected by that.

Also observed during recovery: a daemon **auto-respawned mid-prune**. Plan around
that when doing maintenance. And the event log refilled at ~4.5 events/s
immediately after restart — the loop is untouched until defect 1 lands.

> **Correction.** An earlier revision of this document attributed that respawn to
> `ensure_hangar_daemon` firing on "any `ainb` CLI invocation". That is wrong.
> `ainb-core/src/main.rs:164-172` gates the call inside
> `Some(("tui", _)) | None =>` — it fires **only** when `ainb` launches the TUI
> (bare `ainb` or `ainb tui`), never on `ainb hangar …`, `ainb run`, `ainb status`
> or any other subcommand. A fleet of agents shelling `ainb <subcommand>` never
> touches that path. The observed respawn is real; the cause I assigned to it was
> not. Verified by reading the match arm directly.

## Defect 4's fix already exists — audited 2026-08-08

Branch `fix/hangar-daemon-single-instance` (11 commits, +2131/-97) implements it.
**Unmerged, no PR.** `single_instance.rs` is absent from `main`. That is why the
95-daemon incident recurred: the fix was written, tested, and left on a branch.

Design: replace the end-of-boot pidfile *write* with a start-of-boot lock
*acquisition* — `single_instance::acquire(&dir)` as the first statement of
`boot()` (`lib.rs:433`), taking `BdLock` on `<hangar_home>/hangar/daemon.lock`.
Outcomes are `Acquired` (hold the guard for process life), `HeldBy(pid)` (refuse,
name the owner), `Contended` (sample once more, then refuse). Plus a holder
predicate that checks `ps -p <pid> -o args=` as well as liveness (a pid in a file
outlives a reboot and gets recycled), compare-and-delete release, SIGTERM
handling, and a `watch_ownership` watchdog that stands down if the lock is taken
away.

Independent adversarial audit (Codex, gpt-5.3) — **verdict: ship with changes.**
The boot-time race is closed and proven by concurrent-thread tests that assert
zero overlap under a `Barrier`, not merely "no panic": `hard_link` publish is
kernel-atomic (EEXIST) and `steal_atomically`'s `rename` is winner-take-all.

Residual findings, ranked:

| # | Sev | Finding |
|---|---|---|
| 1 | med-high | The 30 s watchdog reopens the same double-hold on operator error. Delete `daemon.lock` (or restore the home from backup) while the incumbent lives, and a second daemon boots and `rpc::bind` unlinks the incumbent's socket *immediately*, but the incumbent keeps its claim loop and sweepers on the same SQLite db for up to a full tick. Recommend a lower production default or an fs-event watch. `lib.rs:762-773`, `single_instance.rs:123-132` |
| 2 | low | `ps_args()` has no timeout and runs synchronously as the first statement of `boot()`, not on a blocking pool. A wedged `ps` hangs boot indefinitely, defeating the module's own fail-fast intent. Wrap with a short timeout, failing safe as the `None` case already does. `single_instance.rs:105-117` |
| 3 | low | Double-sampling `Contended` narrows but does not eliminate "every contender declines, home ends up empty" — it is two more 500 ms windows of the same mechanism. Only reachable under 3+-way simultaneous boot, which cannot loop. Fine as-is. `single_instance.rs:66-77` |
| 4 | latent | `is_hangar_daemon_args` would not recognise an **in-process** `boot()` inside a cargo-test binary (argv is `target/…/deps/<hash>`, matching neither the sidecar basename nor the `["hangar","daemon","run"]` window). Nothing does that today — every daemon test spawns a real subprocess — so it is a trap for a future test author, worth a doc comment not a code change. `single_instance.rs:218-226` |

Test isolation (the question that prompted the audit) — **verified clean**:
`boot()` has a single production call site (`cli/hangar/mod.rs:8461`) and always
resolves the home through `paths::hangar_home()`, with no bypass. `--once` takes
the lock too (`lib.rs:433`, before the `once` branch at `:730`) and releases it on
early return. Every daemon test spawns a real `ainb-hangar-daemon` subprocess
against its own `tempfile::tempdir()`, with `AINB_HANGAR_HOME`/`AINB_HOME`
explicitly cleared and `HOME` pinned — nextest process-parallelism cannot make two
share a home. A macOS `ps -o args=` truncation concern was tested directly with a
268-char argv and found not to apply on this box.

On over-engineering (`flock` vs this): the ps-argv identity layer — roughly a
third of `single_instance.rs` — exists only because a numeric pid in a file
outlives a reboot. A `flock`/`fcntl(F_SETLK)` held for process lifetime makes the
kernel ground truth, so crash and reboot release it for free and most of that
machinery disappears. It would **not** remove the watchdog: a deleted or replaced
lock file is a different inode under any primitive. Reusing the crate's existing,
already-tested `BdLock` is a defensible pragmatic call, but it is genuinely more
machinery than the OS primitive it stands in for.

## Open Questions

- Is `beads_sync::sync_loop` wired into `boot()` at all? No `run_inbound_loop`
  call site was found (defect 6g). 30-second confirm.
- What is `HANGAR_SWEEP_INTERVAL_MS` set to on this host? Defect 6e is only a
  hazard if it is small.
- Relative weight of defect 2 vs 2b in the RSS curve is not separated. Both are
  confirmed mechanisms; which dominates needs a profile, not more reading.
- **Any measurement must isolate a single daemon.** Four are live, plus stray
  test daemons observed at 83% and 92% CPU with RSS 466 MB / 285 MB. The 469 MB
  "baseline" in the original report matches one debug-build daemon exactly, so
  the 469 → 1151 MB curve may be conflating processes.
