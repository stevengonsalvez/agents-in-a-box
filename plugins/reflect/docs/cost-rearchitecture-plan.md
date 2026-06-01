# Reflect Cost Re-architecture — Implementation Plan

> **Status**: DRAFT for review (Stevie) — no code changed yet.
> **Date**: 2026-05-31
> **Trigger**: drain session `5ff8b14d` burned **41.5M tokens in 9.6 min** (claude-opus-4-8) for **zero net-new learnings**.
> **Decisions locked**: model strategy = **cascade + weekly Opus synthesis**; build **all four** workstreams; **plan-first** (this doc) before implementation.

---

## 0. Diagnosis recap (what we're fixing)

Session `5ff8b14d` (cwd `d/git/research-tech`) was a background `reflect-drain-bg.sh` spawn running `claude -p "/reflect <transcript>"` on a cochilli transcript (`415fd67e`).

| symptom | measured | root cause |
|---|---|---|
| 41.5M tokens / 9.6 min | 223 Opus turns × ~176K ctx | unbounded agentic loop on a fat context |
| full transcript in context | analyzed transcript = 493 KB ≈ **123K tok**, `cat`'d in | skill ingests whole transcript, no slicing |
| caching dead | `cache_read` frozen at **21,670** every turn; `cache_creation` grew 59K→199K, re-paid each turn @2× (1h) | only the static system head cached; volatile injection above the transcript busts the rest (hypothesis) |
| same transcript reflected **16×** | 16 distinct `claude -p` sessions today | no enqueue dedup, no processed-set |
| daily cap 20 → **61** events | race in cost-count check | non-atomic rate limit |
| 223 turns despite `--max-turns 25` | run ended `end_turn` @575s, just under 600s timeout | turn cap not honored/applied; only wall-clock bounded it |
| 0 net-new learnings | transcript was reflect-on-reflect | no skip-gate for already-harvested / no-signal transcripts |

**Cost lesson**: the dominant lever is **context × turns × cache-miss**, *not* model price. Opus→Sonnet alone ≈ 5×; fixing context/turns/cache ≈ 20–50×. Do both.

### Current topology (what exists in `plugins/reflect/`)

```
 PRODUCERS (hooks, no LLM)                 QUEUE                 CONSUMERS
 ┌──────────────────────────┐   append   ┌──────────────┐
 │ precompact_reflect.py     │ ─────────▶ │ ~/.reflect/  │ ◀── sessionstart_drain_reflections.py
 │   (no dedup)              │            │ pending_     │       (SURFACER → additionalContext)
 │ stop_reflect.py           │ ─────────▶ │ reflections  │ ◀── reflect-drain-bg.sh
 │   (dedups vs queue)       │            │ .jsonl       │       (BG DRAINER → claude -p, the costly path)
 └──────────────────────────┘            └──────────────┘
```

Two producers, two consumers, flat append-only queue, no idempotency, agentic Opus consumer with Bash and no real budget. Reusable assets already present:
- `scripts/signal_detector.py` — regex correction detection, HIGH/MED/LOW confidence, `detect_signals()` + `deduplicate_signals()`. **This is the skip-gate engine ($0, no LLM).**
- `scripts/reflect_db.py` — sqlite; tables `events`, `learnings`, `sources`; `compute_content_hash()`, `get_known_content_hashes()`. **Dedup + observability sink.**
- `scripts/reflect_timeline.sh` — statusline dashboard reading `drain.log`. **Extend for cost.**
- `hooks/reflect-drain-bg.sh` — the consumer to harden.

---

## 1. Target topology

```
 PRODUCERS ──▶ enqueue() ──┐
   precompact   (dedup +    │      ┌──────────────────────────────┐
   stop          signal-    ├────▶ │ idempotent queue (sqlite)    │
                 gate at    │      │  key=transcript_hash         │
                 source)    │      │  status: pending|processing| │
                            │      │          done|skipped|poison │
                            │      └───────────────┬──────────────┘
                            │                      │ one worker, debounced
                            ▼                      ▼
                    [single consumer]      ┌──────────────────────────────────┐
                    (kill surfacer OR bg)  │ CASCADE (per transcript)          │
                                           │  1 Haiku/regex GATE  → skip $0     │
                                           │  2 Sonnet EXTRACT on slices        │
                                           │  3 embed DEDUP (no LLM)            │
                                           │  4 Sonnet/template WRITE           │
                                           │  5 Opus ESCALATE (rare)            │
                                           │  + hard caps: turns≤8 / 120s / $   │
                                           └───────────────┬───────────────────┘
                                                           ▼
                                           events table ──▶ `reflect cost` + timeline + langfuse
                                                           ▼
                                           reindex = SEPARATE self-healing batch job
```

---

## 2. Workstreams

Four workstreams, sequenced so the **bleeding stops first** (W1+W2 are low-risk guards), then quality (W4 cascade), then the structural rebuild (W5). W3 (observability) runs alongside to measure the wins.

### W1 — Circuit breaker (stop the bleeding) — `hooks/reflect-drain-bg.sh`

Belt-and-suspenders; never trust a single cap.

| guard | change | default |
|---|---|---|
| turn cap | keep `--max-turns` **and** verify it's honored; lower it | `REFLECT_DRAIN_MAX_TURNS=8` |
| wall timeout | already 600s — lower for a bounded task | `REFLECT_DRAIN_TIMEOUT=180` |
| token budget kill | NEW: poll output / wrap; abort entry if est. tokens > ceiling | `REFLECT_DRAIN_TOKEN_MAX=2_000_000` |
| atomic rate limit | replace grep-count with `flock` + counter file; fixes 61>20 race | `REFLECT_DRAIN_DAILY_MAX=20` |
| spawn debounce | only spawn if last drain > N min ago (timestamp file) | `REFLECT_DRAIN_DEBOUNCE_SEC=600` |
| kill switch | honor `REFLECT_DISABLED=1` in every hook + script | — |
| model flag | `REFLECT_DRAIN_MODEL` (so we can pin Sonnet without code change) | `sonnet` |

**Token budget kill** detail: `claude -p` only returns usage at the end, so mid-run we rely on `--max-turns` + timeout as the hard stops; the token ceiling is a *post-hoc* poison trigger (if a completed run reports > ceiling, poison the transcript so a retry never repeats it). Mid-run hard stop = turns + wall-clock.

**Acceptance**: a synthetic "rabbit-hole" transcript cannot exceed N turns / wall-clock / once-per-debounce-window; daily cap holds under concurrent spawns (flock test).

### W2 — Skip-gate + queue dedup (kill the waste at source) — producers + drainer

Highest waste-elimination per line. Reuses `signal_detector.py`.

1. **Enqueue gate** (`precompact_reflect.py`, `stop_reflect.py`): before appending, run `signal_detector.detect_signals(transcript_text)`. If **no HIGH/MEDIUM** signal → don't enqueue (log `skipped:no-signal`). Also drop **reflect-on-reflect** transcripts (first user message starts with `/reflect` / contains the drain prompt) — this alone would have skipped `415fd67e`.
2. **Enqueue dedup**: key each entry by `transcript_path` (or content hash via `reflect_db.compute_content_hash`). `precompact_reflect.py` currently has **no dedup** — add the same queue-scan `stop_reflect.py` already does, and additionally check the processed-set.
3. **Processed-set**: drainer checks `reflect_db` `events` (or cost log) before processing — if this transcript already `done`, skip. Kills the 16× reprocessing.

**Acceptance**: feeding the same transcript twice enqueues once; a clean-success / reflect-on-reflect transcript never reaches an LLM; replay of today's queue processes each transcript ≤1×.

### W3 — Observability (measure it) — `reflect_db` events + `reflect cost` + timeline + langfuse

The drainer already parses `total_cost_usd` from the `claude -p` JSON — it just discards the token breakdown.

1. **Persist full envelope** per run into `reflect_db` `events` (structured) — `{ts, transcript_hash, model, turns, cost_usd, input, output, cache_read, cache_creation, outcome}`. Replaces/augments the flat `drain-cost.jsonl`.
2. **`reflect cost` CLI** (new `scripts/reflect_cost.py`): `--since 7d`, `--by day|transcript|model|outcome`; totals, cached-vs-uncached split, top spenders, dup count, outlier flags (turns>20 or cost>$2).
3. **Extend `reflect_timeline.sh`**: add a cost/token row (cached vs uncached) to the statusline dashboard.
4. **Langfuse**: emit each run as a trace (the `claude-langfuse` integration already exists) → free timeline dashboards.
5. **Backfill**: one-off `scripts/backfill_costs.py` reconstructs history from `~/.claude/projects/**/*.jsonl` (the exact method used to find `5ff8b14d`), so the timeline starts populated.

**Acceptance**: `reflect cost --since 30d --by day` shows token+$ by type; the 41.5M spike on 2026-05-31 is visible; outlier flag fires on `5ff8b14d`.

### W4 — Cascade extraction (cut tokens 20–50×) — `skills/reflect/SKILL.md` + new pipeline

Replace the free-roaming Opus+Bash agent with a bounded, tiered pipeline. **No unrestricted Bash** (that's what enabled the 149-call GraphRAG spelunk).

```
 transcript
   │  STAGE 1  GATE        signal_detector.py ($0)  ── no HIGH/MED → SKIP
   ▼
   │  STAGE 2  EXTRACT     Sonnet, fed ONLY the sliced correction
   │                       exchanges (~5–15K), NOT the 123K transcript.
   │                       Output = structured JSON candidates (schema).
   ▼
   │  STAGE 3  DEDUP       embed candidates → vector + reflect_db
   │                       content_hash search; cosine>0.85 → merge/skip ($0 LLM)
   ▼
   │  STAGE 4  WRITE       Sonnet/template → learning doc + entity sidecar
   ▼
   │  STAGE 5  ESCALATE    Opus ONLY for "subtle + high-value + low-confidence"
                           (rare; flagged by stage 2)
```

- **Slicing** is the big token win: stage 2 never sees the whole transcript — `signal_detector` returns the offending exchanges; feed those ± a few turns of context.
- **Structured output** (JSON schema) removes agentic looping → bounded ~3–5 calls total.
- **Model via env** so Opus stays available for escalation but isn't the default.

**Weekly Opus synthesis** (new `scripts/reflect_synthesis.py`, scheduled): batch job that reads the week's new learnings, merges near-duplicates, and proposes cross-KB meta-patterns. This is where Opus earns its keep — periodic, bounded, high-value.

**Acceptance**: reflecting a real correction-bearing transcript costs <2M tokens (vs 41.5M) and produces the same/better learnings; reflect-on-reflect costs $0 (gated).

### W5 — Structural rebuild (durable)

1. **Idempotent queue** → sqlite (extend `reflect_db`): `queue(transcript_hash PK, path, status, enqueued_at, attempts, last_outcome)`. Replaces flat JSONL.
2. **Single consumer**: pick one — keep the **bg drainer** (does real capture), retire the **surfacer** (the dual-consumer pollution noted previously), or vice-versa. *Decision needed (see §4).* 
3. **Decoupled worker**: move drain trigger off per-SessionStart to a debounced timer (launchd/cron) with a global `flock`. No thundering herd.
4. **Reindex split**: extract GraphRAG reindex into its own resilient batch job with a **graphml validate/repair** step (the `not well-formed: line 38163` doubled-close-tag bug must self-heal, not become a 200-turn agent investigation).
5. **Neutral cwd**: run reflect in a fixed dir (the KB dir), not the triggering session's cwd.

**Acceptance**: queue is idempotent across crashes; exactly one consumer; reindex corruption is logged + auto-repaired, never escalated into the reflect loop.

---

## 3. Sequencing & risk

```
 W1 circuit breaker ─┐
 W2 skip-gate+dedup ─┼─▶ (stops bleeding; low risk; ship first)
 W3 observability ───┘        │
                              ▼
 W4 cascade ───────────────▶ (quality; medium risk; behind a flag)
                              ▼
 W5 structural rebuild ────▶ (durable; higher risk; after W1–W4 prove out)
```

- **W1+W2+W3** are additive guards/measurement — low blast radius, ship together first.
- **W4** changes capture behavior — gate behind `REFLECT_CASCADE=1`, A/B against current on a sample of transcripts before default-on.
- **W5** touches the queue format + scheduling — do last, with a migration from JSONL→sqlite and a fallback.
- All changes mirror to deployed locations (`~/.claude/skills/reflect`, `~/.claude/scripts/`, plugin cache) via the existing bootstrap/sync path — **source-of-truth edits in `plugins/reflect/` only**, then deploy.
- Tests live in `plugins/reflect/tests/`; add coverage for gate, dedup, caps, cost logging.

## 4. Open decisions for Stevie

1. **Which consumer to keep** — bg drainer (headless capture) vs SessionStart surfacer (in-session prompt)? Recommend: keep **bg drainer**, retire surfacer (avoids dual-consumer pollution; capture happens without polluting your live session context).
2. **Scheduler for the decoupled worker** — launchd (mac-native, survives reboot) vs cron vs keep debounced-SessionStart? Recommend **launchd** timer.
3. **Cascade roll-out** — A/B sample size before default-on?
4. **Backfill window** — how far back to reconstruct cost history (30d / 90d / all)?

## 5. Won't-do (scope guard)

- No new "cost dashboard web app" — `reflect cost` CLI + statusline + langfuse is enough.
- No rewrite of the GraphRAG engine — only add a graphml validate/repair guard.
- No change to `recall`/retrieval path — out of scope.
