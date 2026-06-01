# Changelog

All notable changes to the **reflect** plugin. Format follows
[Keep a Changelog](https://keepachangelog.com/); this project uses semantic
versioning.

## [4.0.0] — 2026-05-31 — Cost rearchitecture

Major bump for the drain cost rearchitecture (W1–W5). Triggered by an incident:
a single background `/reflect` drain run burned **41.5M tokens in 9.6 min
(~$713)** for **zero net-new learnings** — it handed a 123K-token transcript to
an Opus agent that roamed for 223 turns, the same transcript was reflected 16×,
and the daily cap was blown 20→61. A 30-day backfill showed reflect was burning
~1.2B tokens / 446 runs / ~$7k, almost all on Opus.

> Note: `4.0.0` was previously sketched for the "universal cross-harness
> install" effort (see `docs/design-records/2026-04-23-v4-universal-install-spec.md`);
> that effort now targets **5.0.0**.

### Added
- **`reflect cost` CLI** (`scripts/reflect_cost.py`) — drain spend by
  day/transcript/model/outcome with the cached-vs-uncached token split,
  outlier flagging, and an approximate per-model $ estimate.
- **`/reflect:cost` sub-skill** (`skills/cost/`) — slash-command wrapper over
  the cost reporter: parses a window (default 1 day), renders the
  cached/uncached/io split by outcome+model+transcript, and falls back to the
  backfill when historical events predate the v4 token envelope.
- **`scripts/backfill_costs.py`** — reconstruct the cost timeline from existing
  `~/.claude/projects` logs into a separate `drain-cost-backfill.jsonl`.
- **Enqueue skip-gate + dedup** (`scripts/reflect_gate.py`) — $0 regex over a
  transcript's dialogue: skips reflect-on-reflect / no-signal / clean sessions
  and anything already queued or processed.
- **Cascade** (`scripts/reflect_cascade.py`) — gate + slice the transcript to
  just signal-bearing windows (~10× smaller) before `/reflect`.
- **graphml self-heal** (`scripts/graphml_repair.py`) — validate + repair the
  doubled-close-tag corruption that caused the incident's rabbit hole; run
  before reindex.
- **`scripts/regate_backlog.py`** — re-gate the existing pending queue (a real
  dry-run collapsed 114 entries → 13).
- **Weekly Opus synthesis** (`scripts/reflect_synthesis.py`) + **launchd
  templates** (`launchd/com.reflect.{drain,synthesis}.plist`).
- **Cost-event envelope** — `drain-cost.jsonl` now records tokens, cost, turns,
  model, and the input/output/cache_read/cache_creation split per run.

### Changed
- **Default drain model is now `sonnet`** (was Opus / unset) via
  `REFLECT_DRAIN_MODEL`. Opus is reserved for rare escalation + weekly synthesis.
- **Hard caps tightened**: `--max-turns` 25 → **8**, per-entry timeout 600s →
  **180s**, plus a **post-hoc token-budget poison** (`REFLECT_DRAIN_TOKEN_MAX`,
  default 2M) so a completed-but-expensive run can never be retried.
- **Atomic `mkdir` lock** replaces the check-then-write PID file (the race that
  let concurrent spawns each pass the daily cap). Daily cap now **sums the
  `entries` field** so $0 skips never consume budget.
- **Debounce** (`REFLECT_DRAIN_DEBOUNCE_SEC`, default 600) collapses a burst of
  session starts to one drain.
- **Neutral cwd** (`REFLECT_DRAIN_CWD`, default `$HOME`) — the drain no longer
  runs `/reflect` inside whatever repo triggered it.
- Producers (`precompact_reflect.py`, `stop_reflect.py`) run the gate before
  enqueue.
- `DRY_RUN` is now side-effect-free (no longer triggers a real reindex).

### Removed / Retired
- **SessionStart surfacer** (`sessionstart_drain_reflections.py`) retired to a
  no-op. It was never wired in `plugin.json`; the background drainer is the sole
  queue consumer (ends the dual-consumer queue pollution).

### Added (env vars)
`REFLECT_DRAIN_TIMEOUT` · `REFLECT_DRAIN_TOKEN_MAX` · `REFLECT_DRAIN_MODEL` ·
`REFLECT_DRAIN_DEBOUNCE_SEC` · `REFLECT_DRAIN_CWD` · `REFLECT_DRAIN_CASCADE` ·
`REFLECT_DISABLED` (kill switch) · `REFLECT_DRAIN_SKIP_REINDEX` (tests).

### Deferred
- Full sqlite-queue migration — W2 dedup (path + processed-set) and W4
  signal-hash already deliver idempotency in practice; the sqlite rewrite is
  robustness polish at the highest migration risk, tracked as a follow-up.

## [3.6.0] and earlier

See `docs/design-records/` for frozen plans (v3.2 single-PR, v4 universal
install) that informed earlier evolution.
