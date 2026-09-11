# Spike 8: SQLite write path at scale

Spec row: `docs/plans/2026-09-11-multi-surface-decisions-spec.md:202`.
Gates: T0-daemon, W0-wire.

**Question.** With 100 live sessions and hook events at 10/s aggregate, does one
SQLite transaction per event (status apply + ledger insert + outbox append) hold
p99 commit latency under 50 ms, and how does WAL grow over an hour with
retention running? If p99 exceeds 50 ms, does batching to one transaction per
drain tick fix it?

**Verdict.** The 50 ms p99 gate passes with roughly 4x headroom, and it still
passes at ten times the specified event rate. Batching to a drain tick is not
needed and actively hurts: it adds about 10 ms of mean staleness while
coalescing almost nothing, because at 10/s a 16 ms tick holds 1.08 events on
average. Keep one transaction per event. [fact]

WAL does not grow. It sits flat near 4.1 MB for the whole run, held there by
SQLite's default `wal_autocheckpoint` of 1000 pages, not by the retention job's
explicit checkpoint. [fact]

The real risk this spike surfaced is not latency, it is the size of the ledger
tables. `fleet_action_receipt` and `attention` have no retention path at all,
and at 10/s sustained the `fleet_event` payload corpus outruns the daemon's
1 GB payload ceiling in hours. See "Decision input". [fact]

---

## 1. Environment

| Item | Value |
|---|---|
| Host | Linux 6.8.0-137-generic, x86_64 |
| CPU | AMD EPYC-Rome, 8 cores visible |
| Disk | `/dev/sda1`, `ROTA=0`, so non-rotational (SSD or NVMe, virtualised) |
| Filesystem | single `/dev/sda1` mount covering `/tmp` and `/home` |
| rustc | 1.96.0 (ac68faa20 2026-05-25) |
| SQLite | 3.46.0, the copy bundled through `sqlx` 0.8.6 / `libsqlite3-sys` |
| Free disk at start | 34 GB on `/`, above the 15 GB abort threshold |
| Bench binary | release profile, `CARGO_TARGET_DIR` under the scratch root |

Note the SQLite version gap: the bench links the 3.46.0 that `sqlx` bundles,
which is also what the daemon links, so it is the honest number. The host's
`sqlite3` CLI is 3.53.3 and was used only for post-run table inspection. [fact]

## 2. Harness: the real crate, not a toy

`ainb-hangar-store` is a path dependency of the bench crate. All 96 migrations
are applied through the crate's own public `apply_migrations`
(`ainb-tui/crates/ainb-hangar-store/src/lib.rs:123`), so the DDL, indexes and
CHECK constraints are exactly the shipped schema, including the partial indexes
that make these writes more expensive than a bare insert. [fact]

Transaction boundaries are the bench's own, because the spike needs all four
writes in one transaction and the function that does that inside a caller-owned
transaction, `FleetRepo::apply_event_in_tx`
(`ainb-tui/crates/ainb-hangar-store/src/repo/fleet.rs:526`), is `pub(crate)`.
Every statement executed is copied verbatim from the repo; provenance below.
[fact]

### PRAGMAs

Replicated verbatim from `Store::open_in`
(`ainb-tui/crates/ainb-hangar-store/src/store.rs:71-90`), with `synchronous` made
a flag so runs D and E can compare NORMAL against FULL.

| Setting | Value | Provenance |
|---|---|---|
| `foreign_keys` | ON | `store.rs:76` |
| `journal_mode` | WAL | `store.rs:77` |
| `synchronous` | NORMAL (1) | `store.rs:88` |
| `busy_timeout` | 10000 ms | `store.rs:89` |
| pool max connections | 10 (sqlx default, never set) | `store.rs:90` |
| `wal_autocheckpoint` | 1000 pages (SQLite default, never set) | absent from the repo |
| `page_size` | 4096 (SQLite default, never set) | absent from the repo |

Runtime confirmation from every run's header line: `sqlite=3.46.0
journal_mode=wal synchronous=1 busy_timeout=10000 wal_autocheckpoint=1000
page_size=4096 pool_max=10`. [fact]

`wal_autocheckpoint=1000` at 4096 bytes per page is a 4.10 MB soft ceiling on the
WAL, and it is the single reason the WAL series below is flat. It is a SQLite
default the store never states. [inference, from the measured 4.1 MB plateau
matching 1000 pages exactly]

### Statements

| Purpose | Provenance |
|---|---|
| `BEGIN IMMEDIATE` | `repo/fleet.rs:38` |
| retry on lock contention, 5 attempts, jittered backoff doubling from 2 ms | `repo/fleet.rs:45`, `repo/fleet.rs:98` |
| contention codes 5, 6, 261, 262, 517 | `repo/fleet.rs:85-93` |
| prior-event read `SELECT ... FROM fleet_event WHERE event_id = ?` | `repo/fleet.rs:1323` |
| session read `SESSION_SELECT_BY_KEY`, all 34 columns | `repo/fleet.rs:1259` |
| outbox append `INSERT INTO fleet_event (...)` | `repo/fleet.rs:585` |
| status apply `UPDATE fleet_session SET ...`, 33 binds plus key | `repo/fleet.rs:1604` |
| session seed `INSERT INTO fleet_session (...)` | `repo/fleet.rs:1569` |
| ledger `INSERT INTO fleet_action_receipt ... ON CONFLICT DO UPDATE` | `repo/fleet.rs:1185` |
| attention raise `INSERT OR IGNORE INTO attention (...)` | `repo/attention.rs:217` |
| attention close `UPDATE attention SET state = 'answered' ...` | `repo/attention.rs:352` |
| reader cursor `SELECT ... WHERE revision > ? ORDER BY revision ASC LIMIT ?` | `repo/fleet.rs:1104` |
| retention delete `DELETE FROM fleet_event WHERE revision IN (...)` | `repo/fleet_retention.rs:119` |
| `PRAGMA wal_checkpoint(PASSIVE)` | `repo/fleet_retention.rs:153` |

Schema provenance: `migrations/0024_event_log.sql:47` (`event_log`),
`migrations/0025_attention.sql:57` (`attention`),
`migrations/0044_fleet_control_plane.sql:12` (`fleet_session`), `:70`
(`fleet_event`), `:88` (`fleet_action_receipt`), plus the later alters that this
write path pays for: `0068` (request fingerprint index), `0072`
(`active_work_count`), `0080` (`payload_evicted_at` and two partial indexes),
`0081` (`observed_at, revision` index), `0084` (unique partial index on open
attention request keys), `0085` (model columns). [fact]

### What one transaction did

```
BEGIN IMMEDIATE
  SELECT  fleet_event   WHERE event_id = ?          (idempotency probe)
  SELECT  fleet_session WHERE session_key = ?       (34 columns)
  INSERT  fleet_event                               (d) outbox, db assigns revision
  UPDATE  fleet_session                             (a) status apply, version +1
  INSERT OR IGNORE attention  | UPDATE attention     (b) 20% of events
  INSERT  fleet_action_receipt ON CONFLICT DO UPDATE (c) ledger
COMMIT
```

Workload shape:

- 100 `fleet_session` rows seeded, identity `sess-0000` to `sess-0099`.
- Poisson arrivals (exponential inter-arrival, xorshift64\* source), session
  drawn uniformly over the 100.
- 20% of events touch the attention projection; one quarter of those close an
  open row rather than raise one, so both the insert and the update shape run.
- Every event writes one `fleet_action_receipt` row with a distinct
  `request_id`, `expected_version`, `idempotency_key` and status `DELIVERED`.
- Payload is a realistic `PostToolUse` hook body, about 1.8 KB. The real corpus
  mean is 8.3 KB (`daemon/src/fleet_retention.rs:81`), so this bench
  understates payload bytes by roughly 4.6x. Called out again in section 6.
- 4 concurrent writer tasks share one pool, modelling the daemon's several
  writer loops (hook ingest, tmux reconciler, provider pollers) that
  `repo/fleet.rs:29-34` names as the source of its historical write storm.
- Reader thread: cursor read every 250 ms, limit 200.
- Retention every 60 s: delete events older than a 30 s rolling window, looping
  in 500-row batches until the sweep returns 0, then `wal_checkpoint(PASSIVE)`.
  30 s rather than production's 48 h/30 d so the sweep actually runs inside a
  20-minute window.
- WAL and main db file sizes sampled every 10 s.

All latencies are measured around the whole transaction (`BEGIN IMMEDIATE`
through `COMMIT` returning) and, separately, around `COMMIT` alone. The gate
metric reported below is the whole transaction, which is the number a drain
budget has to live with.

## 3. Runs

All six runs completed with exit code 0. Raw output:
`scratchpad/spike8/results.log`. Bench source: `scratchpad/spike8/src/main.rs`.

| Run | Shape | Rate | Duration | synchronous |
|---|---|---|---|---|
| A | one transaction per event | 10/s | 20 min | NORMAL |
| B | one transaction per 16 ms drain tick | 10/s | 10 min | NORMAL |
| C | one transaction per event | 100/s | 5 min | NORMAL |
| D | one transaction per event (NORMAL control) | 10/s | 3 min | NORMAL |
| E | one transaction per event | 10/s | 3 min | FULL |
| F | three transactions per event (today's daemon shape) | 10/s | 5 min | NORMAL |

Run F is not in the brief. It measures what the daemon does today, because the
single transaction the spike proposes is not what ships: `FleetRepo::apply_event`
commits the status apply and the outbox append together
(`repo/fleet.rs:495`), while `AttentionRepo::insert_if_absent`
(`daemon/src/attention_ingest.rs:610`) and `FleetRepo::upsert_action_receipt`
(`daemon/src/rpc/mod.rs:2475`) each take the pool and so autocommit on their
own. Three commits per event, not one. Without F the spike would compare the
proposal against nothing. [fact]

### Run A: one transaction per event, 10/s, 20 minutes

| Metric | Value |
|---|---|
| transactions committed | 11,898 |
| achieved rate | 9.91 events/s |
| **transaction p50** | **1.00 ms** |
| **transaction p95** | **3.79 ms** |
| **transaction p99** | **11.21 ms** |
| transaction p99.9 | 26.47 ms |
| transaction max | 95.72 ms |
| transaction mean | 1.49 ms |
| commit-only p50 / p99 / max | 0.14 / 8.97 / 24.22 ms |
| arrival-to-commit p99 | 11.22 ms |
| `SQLITE_BUSY` retries | 0 |
| reader cursor read p50 / p99 / max | 0.28 / 3.00 / 19.02 ms |
| reader rows served | 11,898 (no cursor gaps) |
| retention delete sweep, 19 passes, p50 / max | 6.52 / 38.84 ms |
| `wal_checkpoint(PASSIVE)`, 19 passes, p50 / max | 6.11 / 14.18 ms |
| retention rows deleted | 11,085 |
| WAL bytes min / max / final | 3.93 / 4.14 / 4.14 MB |
| main db final | 8.30 MB |
| rows left: `fleet_event` / `attention` / `fleet_action_receipt` | 813 / 1,766 / 11,898 |
| CPU | 15.35 s total over 1201 s wall, 1.3% of one core |

The 95.72 ms maximum is a single sample out of 11,898 and coincides with a
retention sweep plus checkpoint. p99.9 is 26.47 ms, so the tail is genuinely one
outlier rather than a shoulder. [inference, from the p99.9-to-max gap and the
sweep timestamps]

### Run B: one transaction per 16 ms drain tick, 10/s, 10 minutes

| Metric | Value |
|---|---|
| events applied | 5,942 |
| transactions (ticks that had work) | 5,496 |
| achieved rate | 9.89 events/s |
| **events coalesced per transaction, mean** | **1.08** |
| events per transaction p99 / max | 2 / 4 |
| **transaction p50 / p95 / p99** | **1.03 / 2.85 / 9.77 ms** |
| transaction p99.9 / max | 24.69 / 41.04 ms |
| commit-only p50 / p99 | 0.14 / 8.12 ms |
| **arrival-to-commit p50 / p95 / p99 / max** | **9.97 / 17.16 / 20.65 / 55.16 ms** |
| `SQLITE_BUSY` retries | 0 |
| reader cursor read p99 | 2.84 ms |
| retention delete sweep, 10 passes, p50 / max | 14.25 / 115.24 ms |
| checkpoint p50 / max | 7.49 / 20.93 ms |
| WAL max | 4.08 MB |
| main db final | 5.75 MB |
| CPU | 9.37 s over 601 s wall, 1.6% of one core |

Batching buys 1.44 ms at p99 on the transaction itself and pays 9 ms of mean
staleness for it, because at 10/s across 100 sessions a 16 ms window contains
one event. The coalescing ratio is 1.08. [fact]

### Run C: one transaction per event, 100/s target, 5 minutes

| Metric | Value |
|---|---|
| transactions committed | 27,025 |
| achieved rate | 89.79 events/s |
| **transaction p50 / p95 / p99** | **0.97 / 3.68 / 11.14 ms** |
| transaction p99.9 / max | 22.17 / 231.33 ms |
| commit-only p50 / p99 / max | 0.14 / 8.88 / 32.11 ms |
| arrival-to-commit p99 / p99.9 / max | 11.31 / 36.41 / 231.34 ms |
| `SQLITE_BUSY` retries | 0 |
| reader cursor read p99 / max | 2.33 / 6.64 ms |
| retention delete sweep, 5 passes, p50 / max | 83.75 / 248.26 ms |
| checkpoint p50 / max | 5.97 / 8.02 ms |
| retention rows deleted | 24,402 |
| WAL bytes min / max / final | 3.93 / 6.72 / 6.72 MB |
| main db final | 30.57 MB |
| rows left: `fleet_event` / `attention` / `fleet_action_receipt` | 2,623 / 4,036 / 27,025 |
| CPU | 29.71 s over 301 s wall, 9.9% of one core |

Ten times the specified rate leaves p99 unchanged at 11.14 ms. The store is not
close to saturated: CPU is under 10% of one core. [fact]

Two caveats on this run. The achieved rate is 89.79/s rather than 100/s, and
that is the generator, not the store: at a 10 ms mean inter-arrival the
`tokio::time::sleep` per arrival overshoots by roughly the timer granularity.
The 231 ms maximum lands inside a 248 ms retention sweep, which is the honest
interaction to take away: at 100/s the sweep itself becomes the tail, not the
event transaction. [inference, from the sweep duration matching the max]

### Runs D and E: synchronous NORMAL against FULL, 10/s, 3 minutes each

| Metric | D, NORMAL | E, FULL |
|---|---|---|
| transactions | 1,763 | 1,761 |
| transaction p50 | 0.97 ms | 2.30 ms |
| transaction p95 | 2.17 ms | 6.35 ms |
| **transaction p99** | **7.73 ms** | **11.16 ms** |
| transaction max | 18.11 ms | 20.11 ms |
| commit-only p50 | 0.14 ms | 1.50 ms |
| commit-only p95 / p99 | 0.31 / 6.80 ms | 4.32 / 8.55 ms |
| WAL max | 4.08 MB | 4.03 MB |
| main db final | 3.64 MB | 3.63 MB |
| CPU | 2.30 s (1.3%) | 2.17 s (1.2%) |

FULL costs about 1.4 ms per commit at the median, a 10x increase on
commit-only p50, and 3.4 ms at p99. Both pass the gate. The store ships NORMAL
(`store.rs:88`) and the comment there justifies it by lock-hold time under
concurrent writer loops; these numbers support that reasoning without making
FULL unaffordable at this rate. [fact]

## 4. WAL growth

Run A, 20 minutes, sampled every 10 s, shown every 60 s. Bars are scaled to
0 to 10 MB so the flatness is visible against the main file's climb.

```
  t(s)   WAL      main db     WAL |##########| 10MB      main db |##########| 10MB
     0   3.93MB    0.95MB         |####      |               |#         |
    60   3.96MB    2.49MB         |####      |               |##        |
   120   4.02MB    3.40MB         |####      |               |###       |
   180   4.02MB    3.48MB         |####      |               |###       |
   240   4.08MB    3.70MB         |####      |               |###       |
   300   4.11MB    4.31MB         |####      |               |####      |
   360   4.11MB    4.51MB         |####      |               |####      |
   420   4.11MB    4.72MB         |####      |               |####      |
   480   4.11MB    5.02MB         |####      |               |#####     |
   540   4.12MB    5.31MB         |####      |               |#####     |
   600   4.12MB    5.62MB         |####      |               |#####     |
   660   4.14MB    5.95MB         |####      |               |#####     |
   720   4.14MB    6.14MB         |####      |               |######    |
   780   4.14MB    6.42MB         |####      |               |######    |
   840   4.14MB    6.77MB         |####      |               |######    |
   900   4.14MB    6.93MB         |####      |               |######    |
   960   4.14MB    7.34MB         |####      |               |#######   |
  1020   4.14MB    7.66MB         |####      |               |#######   |
  1080   4.14MB    7.93MB         |####      |               |#######   |
  1140   4.14MB    8.24MB         |####      |               |########  |
  1190   4.14MB    8.30MB         |####      |               |########  |
```

WAL sparkline, run A, 3.93 MB to 4.14 MB full scale:

```
▁▁▂▂▄▅▅▅▅▆▆████████████ (flat: total excursion 0.21 MB over 20 minutes)
```

Run C, 100/s, same sampling:

```
  t(s)   WAL      main db
     0   3.93MB    0.95MB
    30   4.82MB    7.77MB
    60   4.82MB   14.70MB
    90   4.82MB   15.95MB
   120   4.82MB   22.99MB
   150   4.82MB   23.14MB
   180   4.82MB   25.48MB
   210   6.72MB   25.64MB
   240   6.72MB   27.81MB
   270   6.72MB   27.98MB
   290   6.72MB   29.57MB
```

Ten times the write rate moves the WAL plateau from 4.1 MB to 6.7 MB, a 1.6x
change against a 10x change in rate. The WAL does not grow with time in either
run; it steps up to a new plateau and stays there. [fact]

The mechanism is `wal_autocheckpoint=1000`, a SQLite default the store never
overrides: 1000 pages at 4096 bytes is 4.10 MB, which is the run A plateau to
within measurement noise. The automatic checkpointer runs on the committing
writer, and at 10/s there is always another commit along shortly to run it. The
retention job's explicit `wal_checkpoint(PASSIVE)` is therefore not what bounds
the WAL here; it fires once a minute and completes in 6 ms. [inference, from the
plateau matching 1000 pages and from checkpoint durations too short to have
moved 4 MB]

This is the opposite of the failure mode `repo/fleet_retention.rs:133-146`
documents, where a continuous backlog sweep starved the automatic checkpointer
and the WAL grew to 2,599 MB RSS. That needs a writer that never pauses; a
steady 10/s with 1.08 events per 16 ms tick pauses constantly. [inference]

## 5. Extrapolation to one hour

The brief caps the long run at 20 minutes, so the one-hour figures are fits, not
measurements. Stated as such.

| Quantity | Measured (run A, 20 min) | Extrapolated to 1 hour |
|---|---|---|
| transaction p99 | 11.21 ms | 11 to 12 ms [inference] |
| WAL size | plateau 4.14 MB, excursion 0.21 MB | 4.1 to 4.4 MB [inference] |
| main db file | 0.95 MB to 8.30 MB | 19 to 20 MB [inference] |
| events committed | 11,898 | ~35,700 |
| `fleet_action_receipt` rows | 11,898 | ~35,700 |

Basis for each:

- **p99 flat.** p99 was 11.21 ms over 20 minutes at 10/s and 11.14 ms over 5
  minutes at 100/s. Latency is not a function of elapsed time or of accumulated
  rows at this scale, because every hot statement rides an index and the working
  set fits the page cache. [inference, two rates agreeing to within 1%]
- **WAL flat.** The plateau is set by a page count, not by elapsed time.
  [inference]
- **Main db linear.** Fitting t=120 s (3.40 MB) to t=1190 s (8.30 MB) gives
  4.58 KB/s, or 16.5 MB/hour, so 3.40 + 16.5 x (3480/3600) is about 19.3 MB.
  The first 120 s are excluded because they include the migration write and the
  file's initial allocation. [inference, linear fit over 18 minutes]

**The main-file growth is not the event ledger.** Retention held `fleet_event` at
813 rows. The growth is `fleet_action_receipt` at 11,898 rows and `attention` at
1,766, neither of which anything deletes, plus free pages that SQLite keeps in
the file because `auto_vacuum` is off. [fact, from the end-of-run row counts]

**The spike's retention window is 2,880x more aggressive than production.** This
bench deletes at 30 s. The daemon evicts payloads at 48 hours and deletes rows at
30 days (`daemon/src/fleet_retention.rs:59`, `:62`), sweeps hourly
(`:100`), catches up every minute while a backlog remains (`:104`), and waits
5 minutes after boot (`:107`). Under production settings nothing in
`fleet_event` is deleted within the first hour, so the one-hour main-file figure
is not 19 MB, it is the full hour of events: about 35,700 rows at this bench's
1.8 KB payload is roughly 63 MB, and at the real corpus mean of 8.3 KB
(`daemon/src/fleet_retention.rs:81`) roughly 290 MB. [inference, row count
measured, payload size from the cited constant]

## 6. Run F and run G

### Run F: three transactions per event, today's daemon shape, 10/s, 5 minutes

The comparison baseline. Section 3 explains why it exists: the shipped code
commits the status apply and the outbox append together and then commits the
attention projection and the receipt separately, so it pays three commits per
event where the spike's shape pays one.

| Metric | F, three transactions | A, one transaction (20 min) | D, one transaction (3 min) |
|---|---|---|---|
| events applied | 2,964 | 11,898 | 1,763 |
| achieved rate | 9.87/s | 9.91/s | 9.78/s |
| per-event p50 | 1.14 ms | 1.00 ms | 0.97 ms |
| per-event p95 | 2.67 ms | 3.79 ms | 2.17 ms |
| **per-event p99** | **10.40 ms** | **11.21 ms** | **7.73 ms** |
| per-event p99.9 | 18.47 ms | 26.47 ms | 17.37 ms |
| per-event max | 29.91 ms | 95.72 ms | 18.11 ms |
| **commit-only p50** | **0.40 ms** | **0.14 ms** | **0.14 ms** |
| commit-only p95 / p99 | 0.97 / 8.56 ms | 0.42 / 8.97 ms | 0.31 / 6.80 ms |
| `SQLITE_BUSY` retries | 0 | 0 | 0 |
| WAL max | 4.04 MB | 4.14 MB | 4.08 MB |
| main db final | 4.30 MB | 8.30 MB | 3.64 MB |
| CPU | 1.5% of one core | 1.3% | 1.3% |

The clean signal is commit-only p50: 0.40 ms for three commits against 0.14 ms
for one, close to the 3x the commit count predicts. [fact]

Do not read the p99 column as "three transactions are faster". F ran 5 minutes
and A ran 20, and p99 on these runs is dominated by whether a sample landed
near a retention sweep, so the run lengths are not comparable at the tail.
Compare F against D and G, the equal-ish length single-transaction runs: p99
7.73 ms and 8.04 ms against F's 10.40 ms. On every comparison of similar
duration the single transaction is the faster of the two, and it is never
slower. [inference, from the run-length confound; the commit-only p50 is the
measurement that is not confounded]

The single transaction is therefore cheaper than the shape that ships, not
dearer. That matters because D14 already locks it for correctness ("`attention`
rows are a projection written by the same single-writer apply path in the same
transaction"), and this spike had to rule out a latency cost for that
correctness win. There is none. [fact]

### Run G: table and index byte accounting, 10/s, 2 minutes

1,196 events applied, p99 8.04 ms, retention deleted 886 rows during the run.
The database was kept and measured with `dbstat` through the host `sqlite3`
3.53.3.

| Object | Bytes | KB | Rows | Bytes/row |
|---|---|---|---|---|
| `fleet_event` (table) | 643,072 | 628 | 310 | 2,074 |
| `attention` (table) | 380,928 | 372 | 183 | 2,082 |
| `fleet_action_receipt` (table) | 151,552 | 148 | 1,196 | 127 |
| `sqlite_schema` | 69,632 | 68 | n/a | n/a |
| `fleet_session` (table) | 53,248 | 52 | 100 | 532 |
| `sqlite_autoindex_fleet_action_receipt_1` | 40,960 | 40 | 1,196 | 34 |
| `idx_fleet_action_receipt_session` | 36,864 | 36 | 1,196 | 31 |
| `sqlite_autoindex_fleet_event_1` | 16,384 | 16 | 310 | 53 |
| `idx_fleet_event_session_revision` | 16,384 | 16 | 310 | 53 |
| `idx_fleet_event_retention_sweep` | 16,384 | 16 | 310 | 53 |
| `_sqlx_migrations` | 16,384 | 16 | 96 | 171 |
| `sqlite_autoindex_attention_1` | 12,288 | 12 | 183 | 67 |

| File-level | Value |
|---|---|
| `page_count` | 887 pages |
| `page_size` | 4,096 B |
| file size | 3,633,152 B, 3.47 MB |
| **`freelist_count`** | **320 pages, 1,310,720 B** |
| **free share of the file** | **36.1%** |

Two readings.

**A `fleet_event` row costs about 2.1 KB at this bench's 1.8 KB payload, and a
receipt costs about 192 B all in** (127 B of table plus 65 B across its two
indexes). The receipt's indexes add 51% on top of its table, because
`request_id` is a TEXT primary key and `idx_fleet_action_receipt_session`
carries `(session_key, updated_at DESC)`
(`migrations/0044_fleet_control_plane.sql:104`). [fact]

**36% of the file is free pages after two minutes.** That is not a leak. SQLite
reuses freelist pages for later inserts, so the file plateaus at its high-water
mark rather than growing from deletes. It does mean the file never shrinks on
its own after a large reclaim. [fact for the measurement, inference for the
reuse behaviour, which is documented SQLite semantics]

## 7. Decision input

### For T0-daemon and W0-wire

```
┌─────────────────────────┐      ┌──────────────────────────┐
│ one tx per event        │ ──▶  │ p99 11.2 ms   GATE PASS  │  keep
│ 10/s, 100 sessions      │      │ 4.5x headroom on 50 ms   │
└─────────────────────────┘      └──────────────────────────┘
┌─────────────────────────┐      ┌──────────────────────────┐
│ one tx per 16 ms tick   │ ──▶  │ p99 9.8 ms, but +9 ms    │  reject
│ same load               │      │ staleness, 1.08 coalesced│
└─────────────────────────┘      └──────────────────────────┘
┌─────────────────────────┐      ┌──────────────────────────┐
│ storage at 10/s         │ ──▶  │ 1 GB payload ceiling in  │  fix
│ production retention    │      │ ~3.6 h; 2 tables unswept │
└─────────────────────────┘      └──────────────────────────┘
```

**1. One transaction per event. Do not batch to a drain tick.** [fact]

| Question | Answer |
|---|---|
| Does one transaction per event hold p99 under 50 ms? | Yes. 11.21 ms over 20 minutes at 10/s |
| Is the gate comfortable? | Yes. 4.5x headroom at 10/s, 4.5x at 100/s |
| Does batching fix anything? | Nothing needs fixing, and batching costs 9 ms of mean staleness to coalesce 1.08 events |
| Is one transaction worse than the three the daemon commits today? | No. Commit-only p50 0.14 ms against 0.40 ms |

The spike's conditional ("if p99 exceeds 50 ms the status store needs one
transaction per drain tick") does not fire. Strike the per-drain-tick
alternative from T0-daemon's scope. The transaction shape D14 already locked for
correctness, with the `attention` projection written inside the apply
transaction, is also the faster of the two. [fact]

Batching stays the right pattern one layer up, in the renderer, which is where
D15 and spike 4 put it. It is the wrong pattern at the store. [inference]

**2. The 50 ms gate is comfortable, and it is not what will bite.** [fact]

p99 is 11.2 ms at 10/s and 11.1 ms at 100/s, on 1.3% and 9.9% of one core. Zero
`SQLITE_BUSY` retries across 47,000 transactions in six runs, with `BEGIN
IMMEDIATE` plus the shipped 10 s `busy_timeout` doing the work
(`store.rs:89`, `repo/fleet.rs:38`). Latency headroom is not the constraint on
T0-daemon. [fact]

**3. `fleet_action_receipt` and `attention` need retention paths. Neither has
one.** [fact]

Grepping the workspace, the only `DELETE` that reaches either table is
`WorkspaceRepo::delete` (`repo/workspace.rs:465`), a tenant teardown.
`AttentionRepo::close_unclaimed_open` (`repo/attention.rs:495`) flips `state` to
`answered`; it never removes a row. `FleetRetentionRepo` covers `fleet_event`
only, and `fleet_provider_retention` covers `fleet_provider_event`. So at 10/s:

| Table | Rows/day at 10/s | Measured bytes/row | Table bytes/day | Retention today |
|---|---|---|---|---|
| `fleet_action_receipt` | 864,000 | 127 (plus ~65 of index) | 166 MB | **none** |
| `attention` | 172,800 (20% of events) | 2,082 (plus index) | 360 MB | **none**, rows only flip to `answered` |
| `fleet_event` | 864,000 | 2,074 at 1.8 KB payload | 1.79 GB | payload 48 h, rows 30 d |

W0-wire's op-id ledger lands in `fleet_action_receipt`
(`migrations/0044_fleet_control_plane.sql:88`), so D18 adds receipt rows on top
of these. The ledger needs a TTL before mobile multiplies the op count, not
after. Suggested shape, by symmetry with what already exists: a
`delete_receipts_before` on the same hourly sweeper, keyed on `updated_at`,
refusing any row whose `status` is still `PENDING`. [inference]

`attention` is the more urgent of the two by bytes, because each row carries a
full request payload. An answered row older than the row TTL has no reader.
[inference]

**4. At 10/s the `fleet_event` payload corpus breaches the 1 GB ceiling in about
3.6 hours, and the eviction ladder bottoms out.** [inference, from measured
rates against the cited constants]

Production constants: payload eviction at 48 h
(`daemon/src/fleet_retention.rs:59`), row delete at 30 d (`:62`), 1 GiB payload
ceiling (`:65`), and a fixed tightening ladder of 24 h, 6 h, 2 h with the 2 h
rung as a hard floor (`:73`).

Hours from empty to the 1 GiB ceiling:

| Rate | At this bench's 1.8 KB payload | At the real corpus mean of 8.3 KB |
|---|---|---|
| 10/s | 14.4 h | **3.6 h** |
| 100/s | 1.4 h | **0.36 h (22 min)** |

Steady-state payload corpus at each ladder rung, 8.3 KB mean:

| Ladder rung | 10/s | 100/s |
|---|---|---|
| 48 h (normal) | 14.3 GB | 143 GB |
| 24 h | 7.2 GB | 72 GB |
| 6 h | 1.8 GB | 18 GB |
| **2 h (floor)** | **0.60 GB, complies** | **5.98 GB, 5.6x over ceiling** |

Read that as: **at 10/s the ceiling machinery works but the daemon lives
permanently at its 2 h floor**, holding two hours of hook payload rather than the
48 the constant advertises. At 100/s the ladder cannot comply at all, because its
floor is deliberately bounded (`:69` explains why: an unbounded search would
converge on deleting everything). [inference]

The 30-day row TTL is the second half of the problem. At 10/s that is 25.9M
rows, and at the measured non-payload cost of roughly 470 B per row (274 B of
table plus about 200 B across its indexes) that is on the order of 12 GB of
`fleet_event` even with every payload blanked. [inference, measured row and
index bytes extrapolated]

Three ways out, cheapest first:

1. Do not put every hook event in `fleet_event`. The corpus is dominated by the
   four `EVICTABLE_EVENT_TYPES` (`repo/fleet_retention.rs:44`), which are
   evictable precisely because nothing reads their payloads. Store those with an
   empty payload from the start, or do not append them at all, and keep the
   outbox for events a subscriber actually replays. This is a T0-daemon
   normalizer decision, and it is the only option that changes the slope rather
   than the constant. [inference]
2. Shorten `PAYLOAD_TTL_MS`. The 48 h constant was fitted to a 102 MB/day
   corpus; at 10/s the corpus is 1.79 GB/day, 17x that. The comment at `:56`
   states the fit explicitly, so the number is honest about being rate-dependent.
3. Raise `PAYLOAD_CEILING_BYTES`. Only defensible if the target box really has
   the disk, and it does nothing about the row count.

Whichever is chosen, the spec's 10/s assumption should be written down next to
the retention constants, because those constants were fitted against a corpus
roughly 70x slower (87k payload rows over about 7 days, `repo/fleet_retention.rs:5`,
is about 0.14 events/s). [fact]

**5. Retention cadence: keep hourly. It is not a latency risk at 10/s, and it is
the p99 tail at 100/s.** [fact]

| Rate | Sweep duration p50 | Sweep max | Rows deleted per sweep | Checkpoint p50 / max |
|---|---|---|---|---|
| 10/s (run A) | 6.52 ms | 38.84 ms | ~585 | 6.11 / 14.18 ms |
| 100/s (run C) | 83.75 ms | 248.26 ms | ~4,880 | 5.97 / 8.02 ms |

Run C's worst transaction, 231 ms, sits inside a 248 ms sweep. At 100/s the
sweep, not the event transaction, owns the tail. [inference, from the durations
coinciding]

Two things make this look worse than production will be. The bench sweeps every
60 s where the daemon sweeps hourly (`daemon/src/fleet_retention.rs:100`), and
the bench loops its 500-row batches back to back where the daemon pauses 50 ms
between them (`:97`) specifically so other writers interleave. So these sweep
durations are a pessimistic bound, and the production shape already contains the
mitigation. No change recommended. [fact]

**6. `auto_vacuum`: no. Periodic `VACUUM`: no. One `VACUUM` after a backlog
drain: yes.** [inference]

36% of run G's file was free pages, and it stays that way, but SQLite reuses
freelist pages for subsequent inserts, so the file plateaus at its high-water
mark instead of growing from deletes. Run A's steady file growth was the two
unswept tables, not vacuum debt.

- `auto_vacuum = FULL` relocates pages on every commit, which puts work on the
  exact commit path this spike is protecting. Rejected.
- `auto_vacuum = INCREMENTAL` cannot be enabled on an existing database without
  a full `VACUUM` anyway, and buys nothing while the high-water mark is the real
  ceiling.
- A periodic `VACUUM` rewrites the whole file and takes an exclusive lock, which
  is the one thing `repo/fleet_retention.rs:147` says a background janitor must
  never do to the read path.

The case where reclaim is worth it is the one-off: the module documents a real
847 MB / 1.1M row backlog (`repo/fleet_retention.rs:4`), and after draining that
the free pages are worth returning. `VACUUM INTO` is the safe form and the store
already uses it for pre-migration backups (`store.rs:230`). Run it once after a
backlog drain, never on a timer. [inference]

**7. `synchronous`: keep NORMAL. It changes latency, not the verdict.** [fact]

| | NORMAL (run D) | FULL (run E) | Delta |
|---|---|---|---|
| commit-only p50 | 0.14 ms | 1.50 ms | 10.7x |
| commit-only p99 | 6.80 ms | 8.55 ms | 1.26x |
| transaction p99 | 7.73 ms | 11.16 ms | +3.43 ms |
| transaction max | 18.11 ms | 20.11 ms | +2.00 ms |

Both pass the gate with room. Keep NORMAL as shipped (`store.rs:88`); the
comment there justifies it by lock-hold time under the daemon's concurrent
writer loops, and a 10x longer median commit is a 10x longer hold of the write
lock those loops queue behind.

One durability consequence W0-wire should note rather than discover: in WAL mode
`synchronous = NORMAL` can lose the most recent commits on OS crash or power
loss, though never on process crash and never with corruption. For D18's op-id
ledger that means a hard power loss can drop a receipt whose action already had
its side effect, so a retried op id would be answered `created` a second time
instead of `replayed`. The idempotency story is sound for process crashes and
reconnects, which is what it is actually for; it is not a power-loss guarantee.
Say so in the W0-wire wording rather than implying stronger. [inference, from
documented WAL/NORMAL semantics plus the receipt's `request_id` primary key at
`migrations/0044_fleet_control_plane.sql:89`]

### Summary table

| Decision | Input from this spike |
|---|---|
| one transaction per event vs per drain tick | **Per event.** Gate passes at 4.5x; batching adds 9 ms staleness and coalesces 1.08 events |
| 50 ms p99 gate | **Comfortable.** 11.21 ms at 10/s, 11.14 ms at 100/s, zero `SQLITE_BUSY` |
| attention projection inside the apply transaction (D14) | **No latency cost.** Cheaper than today's three commits |
| retention cadence | **Keep hourly.** Sweep is 6.5 ms at 10/s; the 50 ms batch pause already covers 100/s |
| `fleet_action_receipt` retention | **Add one.** 864k rows/day at 10/s, nothing deletes it, and D18 adds more |
| `attention` retention | **Add one.** 360 MB/day of table at 10/s, rows only ever flip to `answered` |
| `fleet_event` payload volume | **Reopen the constants.** 1 GiB ceiling in 3.6 h at 10/s; ladder pinned at its 2 h floor; unfixable at 100/s |
| `auto_vacuum` / periodic `VACUUM` | **Neither.** One `VACUUM INTO` after a backlog drain only |
| `synchronous` | **Keep NORMAL.** FULL is affordable but holds the write lock 10x longer at the median |
| store settings to change | None on the hot path. `wal_autocheckpoint` is doing real work at its default and should be stated in `store.rs` rather than left implicit |

## 8. What would falsify this

| Claim | What would break it |
|---|---|
| p99 11.2 ms at 10/s | A slower disk. This is non-rotational with a page cache holding the whole working set. A spinning disk or a network filesystem changes the commit path, not the statement cost |
| WAL stays near 4.1 MB | A writer that never pauses long enough for the automatic checkpointer, which is exactly the backlog case `repo/fleet_retention.rs:133` measured at 2,599 MB RSS |
| batching coalesces nothing | A burstier arrival process. This is Poisson; real hook traffic arrives in per-turn bursts, so a 16 ms tick could coalesce more than 1.08. Worth a re-measure against a recorded hook trace before anyone revisits the per-tick option |
| 100/s holds p99 | The generator only achieved 89.79/s, so 100/s is not strictly demonstrated. The shortfall is timer granularity in the bench, not store backpressure, and CPU at 9.9% of one core leaves no reason to expect a cliff |
| storage extrapolations | All of them assume a sustained rate. They are rate x time x measured bytes, not measurements. The payload mean in particular is this bench's 1.8 KB against the cited 8.3 KB real mean, which is why both columns are shown |

Not measured, and out of scope for this spike: behaviour with a second process
writing the same database (the spike ran one process with 4 writer tasks on one
pool), `fleet_provider_event` traffic, and the ACP adjunct-row path that commits
two tables together.

## 9. Reproducing

```
scratchpad/spike8/src/main.rs       bench, one file
scratchpad/spike8/Cargo.toml        path dep on ainb-hangar-store
scratchpad/spike8/run-all.sh        runs A to E
scratchpad/spike8/results.log       raw output, all six runs plus G
```

```
CARGO_TARGET_DIR=scratchpad/spike8/target cargo build --release
./spike8 --label A --mode per_event --rate 10  --dur 1200 --writers 4 --dir <dir>
./spike8 --label B --mode per_tick  --rate 10  --dur 600             --dir <dir>
./spike8 --label C --mode per_event --rate 100 --dur 300  --writers 4 --dir <dir>
./spike8 --label E --mode per_event --rate 10  --dur 180  --sync full --dir <dir>
./spike8 --label F --mode three_tx  --rate 10  --dur 300  --writers 4 --dir <dir>
```

Flags: `--mode per_event|per_tick|three_tx`, `--rate` events/s aggregate,
`--dur` seconds, `--writers` concurrent writer tasks, `--sync normal|full`,
`--window` retention window in seconds (default 30), `--dir` database directory.

This spike wrote nothing to the repository: every artifact lives under the
scratch root, and `ainb-hangar-store` was consumed read-only as a path
dependency. `git status --short` does show
`docs/plans/2026-09-11-multi-surface-decisions-spec.md` as modified, which is
the team lead folding spike results (1, 4, 8, 9) into spec v1.2 while this
spike ran, not an edit from here.
