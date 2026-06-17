# Changelog

All notable changes to the **reflect** plugin. Format follows
[Keep a Changelog](https://keepachangelog.com/); this project uses semantic
versioning.

## [4.1.0] — 2026-06-17 — Recall upgrade (57 ports)

Minor bump for the recall-upgrade campaign (PR #248): **57 features ported** from
Hindsight, ByteRover, agentmemory, and claude-mem into the recall layer, each
shipped with a real-engine behavioral proof
(`reflect-kb/tests/eval/behavioral/proofs/`). Additive and backward-compatible —
every new arm/signal is gated by a `RECALL_*`/`REFLECT_*` env knob that defaults
off or to the pre-4.1 behavior, and the DB schema migration is additive
(`CREATE TABLE IF NOT EXISTS` on first connection). No existing 4.0 install
breaks; the retrieval improvements are live out of the box.

### Added — retrieval / inject
- **Graph-expansion arm** (R1, `RECALL_GRAPH_ARM`), **cross-encoder rerank** (R2,
  `RECALL_CROSS_ENCODER`, lazy `sentence-transformers` — degrades to the legacy
  formula if absent), **MMR diversity** (R3, `RECALL_MMR`), **token-budget
  retrieval** (R4, `REFLECT_RECALL_MAX_TOKENS`), **temporal arm + query-date
  parsing** (R5/R6, `RECALL_TEMPORAL`), **OOD relevance gate** (R7,
  `--min-overlap`), **bounded multiplicative boosts** (R8, `RECALL_*_ALPHA`),
  **fuzzy cache tier** (R9), **3-tier hierarchical inject + forced-grounding
  short-circuit** (R10/R11, `REFLECT_TIERED_INJECT`), **per-arm calibrated
  thresholds** (R12, `reflect calibrate-thresholds`), **auto-skill-refresh +
  per-skill staleness** (R13/R14), **per-project sharding** (R15), **project
  affinity** (R16), **skills index** (R20), **staged 3-layer recall** (M1).
- See **`docs/retrieval-features.md`** for each feature with a worked example and
  the counterfactual.

### Added — storage / signals / consolidation / open-domain
- Storage: structured drain fields (S1), typed causal links (S2), numeric
  confidence (S3), provenance/proof-count (S4), belief revision (S5), history
  snapshots (S6), chunk-hash dedup (S7), doc→chunk→learning grouping (S8),
  volatile-signals sidecar (S9), write-validate-retry (S10).
- Signals: cross-turn contradiction (SG1), git-event capture (SG2, post-commit
  hook), idle trigger (SG3, launchd), test-outcome (SG4), tool-loop detect
  (SG5), knowledge-gap (SG6), todo-completion (SG7), permission capture (SG8).
- claude-mem: writer breaker (M2), quota-aware abort (M3), pluggable modes (M4),
  commit verification (M5), privacy stripping (M6), corpus Q&A (M7,
  `/reflect:corpus`), token economics (M8).
- agentmemory: pinned slots (A1), bitemporal edges (A2), per-row TTL (A3),
  followup-rate diagnostic (A4), synthetic no-LLM compression (A5), branch-aware
  isolation (A6).
- Open-domain: observations layer (O1), conventions doc (O2), persona fields
  (O3). Consolidation: semantic dedup (C1), auto-consolidation (C2), graph
  maintenance (C3), lifecycle events (C4), KB export/import (C5).

### Infrastructure
- **DB schema**: 14 new `reflect.db` tables (auto-migrated, additive).
- **New `reflect.db` is migrated transparently** on first connection — no manual
  migration step for existing installs.
- **Pre-merge review fixes**: depth-aware `<private>` stripping (M6 nested-tag
  leak), corpus date-filter validation (M7), R14 staleness applied to the inject
  tier, `validate_sidecar` degrades as a library instead of `sys.exit`.

### Activation notes (features that need a one-time wiring step)
- **SG2 git-event capture** needs the post-commit hook installed per repo (see
  Install below). **SG3 idle** and the other launchd timers load via their
  `launchd/*.plist` INSTALL blocks. **S8** document-grouping persists once the
  drain calls the chunk-record step.

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
