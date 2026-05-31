# Specification: Reflect Cost Re-architecture

**Generated from:** `plugins/reflect/docs/cost-rearchitecture-plan.md`
**Interview date:** 2026-05-31
**Version:** 1.0

## Executive Summary

Re-architect the reflect drain pipeline after a background drain burned **41.5M tokens in 9.6 min for zero net-new learnings**. Replace the unbounded Opus+Bash agent with a bounded, tiered **cascade**, gate out worthless transcripts before any LLM runs, dedup the queue, harden with a real circuit breaker, and make spend observable. Eight design decisions are now locked (see "Decisions Made").

## Objectives

### Primary Goals
- Cut per-transcript reflect cost ~20–50× (target p95 run < 2M tokens vs 41.5M).
- Make worthless transcripts (reflect-on-reflect, clean success, no-signal) cost **$0** (gated pre-LLM).
- Guarantee no transcript is processed more than once (idempotent queue + dedup).
- Make a runaway run structurally impossible (hard turn/wall/token caps + atomic rate limit + spawn debounce).
- Make reflect spend visible by timeline (cached vs uncached tokens, $).

### Success Metrics
| metric | today | target |
|---|---|---|
| p95 tokens / reflect run | 41.5M (worst) | < 2M |
| cost of reflect-on-reflect / clean / no-signal | full agent run | $0 (gated) |
| times a single transcript processed | 16× | 1× |
| daily-cap adherence under concurrent spawns | blown (61 vs 20) | holds (flock) |
| consumers on the queue | 2 (race) | 1 |
| cost timeline visibility | none | `reflect cost` + statusline + langfuse |

## Scope

### In Scope
- W1 Circuit breaker, W2 skip-gate + queue dedup, W3 observability, W4 cascade pipeline + weekly Opus synthesis, W5 structural rebuild.
- Source-of-truth edits in `plugins/reflect/`; deploy mirrors after.

### Out of Scope
- Rewriting the GraphRAG engine (only add a graphml validate/repair guard).
- Changing the `recall`/retrieval path.
- A standalone cost dashboard web app (CLI + statusline + langfuse suffice).

### Future Considerations
- Shadow A/B harness for future capture-quality changes (we chose straight-to-default this round).
- Tuning the LOW-signal detector precision once cost telemetry exists.

## Decisions Made (interview output)

```
 PRODUCERS ─▶ enqueue(gate+dedup) ─▶ sqlite queue ─▶ [bg drainer ONLY] ─▶ cascade ─▶ events/cost
                                                          ▲ launchd 10min + flock
                                          (surfacer RETIRED)
```

| # | decision | choice | rationale |
|---|---|---|---|
| 1 | Queue consumer | **Keep bg drainer, retire surfacer** | single consumer, no race, no live-session pollution |
| 2 | Drain scheduler | **launchd timer (~10min) + flock** | reboot-survivable, decoupled from session starts, no thundering herd |
| 3 | Cascade rollout | **Straight to default** | fastest; accept behavior-change risk, telemetry catches misses |
| 4 | Cost backfill window | **30 days** | recent trend, minimal seed |
| 5 | Skip-gate aggressiveness | **Reflect on ANY signal (incl LOW); skip only no-signal / clean-success / reflect-on-reflect** | cheap insurance against missing quiet lessons |
| 6 | 113-entry backlog | **Re-gate through new filter** | dedup+gate collapses it to a handful, then cascade |
| 7 | Dedup strategy | **Hash fast-path + vector fallback** | exact re-runs cheap; KB vector (nano-graphrag, cos>0.85) catches paraphrased dupes |
| 8 | Circuit-breaker caps | **8 turns / 180s / poison >2M tokens** | comfortable headroom over a ~5-call cascade |

## Technical Requirements

### Architecture — target topology

```
 PRODUCERS (hooks, no LLM)
   precompact_reflect.py ─┐  enqueue():
   stop_reflect.py ───────┤   1. reflect-on-reflect? clean? no-signal? ─▶ DROP (log)
                          │   2. signal_detector: ANY signal? ─▶ else DROP
                          │   3. dedup key = transcript_hash; already queued/done? ─▶ DROP
                          ▼
                  ┌──────────────────────────────┐
                  │ sqlite queue (reflect_db)     │
                  │  transcript_hash PK, path,    │
                  │  status pending|processing|   │
                  │  done|skipped|poison, attempts│
                  └───────────────┬───────────────┘
        launchd ~10min + flock ──▶│  (bg drainer; surfacer retired)
                                  ▼
            ┌─────────────────────────────────────────────────────┐
            │ CASCADE per transcript (hard caps: 8 turns/180s/2M)   │
            │  1 GATE     signal_detector ($0)                      │
            │  2 EXTRACT  Sonnet on sliced exchanges (~5–15K)        │
            │  3 DEDUP    content_hash → else embed→KB cos>0.85      │
            │  4 WRITE    Sonnet/template → doc + entity sidecar     │
            │  5 ESCALATE Opus only (subtle+high-value+low-conf)     │
            └───────────────┬─────────────────────────────────────┘
                            ▼
        events table ─▶ reflect cost CLI · statusline row · langfuse
        reindex = SEPARATE launchd batch w/ graphml validate+repair
```

### Components

| component | file(s) | change |
|---|---|---|
| Enqueue gate + dedup | `hooks/precompact_reflect.py`, `hooks/stop_reflect.py` | call `signal_detector`, drop reflect-on-reflect/clean/no-signal, dedup by hash |
| Skip-gate engine | `scripts/signal_detector.py` | reuse `detect_signals()`; add reflect-on-reflect + clean-success detectors |
| Idempotent queue | `scripts/reflect_db.py` | new `queue` table; migrate from `pending_reflections.jsonl` |
| Circuit breaker | `hooks/reflect-drain-bg.sh` | caps (8/180/2M poison), `flock` atomic daily cap, debounce, `REFLECT_DISABLED`, `REFLECT_DRAIN_MODEL` |
| Cascade pipeline | `skills/reflect/SKILL.md` + new `scripts/reflect_cascade.py` | 5-stage tiered pipeline, structured output, no unrestricted Bash |
| Dedup | `scripts/reflect_cascade.py` + `reflect_db.compute_content_hash` + KB vector | hash fast-path then nano-graphrag vector search |
| Weekly synthesis | new `scripts/reflect_synthesis.py` + launchd | Opus batch: merge near-dupes, surface meta-patterns |
| Observability | `reflect_db` `events`, new `scripts/reflect_cost.py`, `scripts/reflect_timeline.sh`, langfuse | persist full usage envelope; `reflect cost` CLI; statusline row; traces |
| Backfill | new `scripts/backfill_costs.py` | parse last 30d of `~/.claude/projects/**/*.jsonl` into `events` |
| Reindex split | new batch + graphml guard | move reindex out of drain loop; validate/repair `not well-formed` corruption |
| Scheduler | new launchd plists | drain (~10min) + weekly synthesis + reindex |

### Env configuration

| var | default | purpose |
|---|---|---|
| `REFLECT_DRAIN_MAX_TURNS` | 8 | hard turn cap |
| `REFLECT_DRAIN_TIMEOUT` | 180 | wall-clock cap (s) |
| `REFLECT_DRAIN_TOKEN_MAX` | 2_000_000 | poison transcript if a completed run exceeds |
| `REFLECT_DRAIN_DAILY_MAX` | 20 | atomic (flock) daily entry cap |
| `REFLECT_DRAIN_DEBOUNCE_SEC` | 600 | min gap between drain runs |
| `REFLECT_DRAIN_MODEL` | sonnet | extract/write model (pin without code change) |
| `REFLECT_DISABLED` | 0 | global kill switch (honored in all hooks + scripts) |
| `REFLECT_GATE_MIN_SIGNAL` | any | gate floor: `any` (incl LOW) per decision #5 |

### Models

| stage | model | id |
|---|---|---|
| extract / write | Sonnet | `claude-sonnet-4-6` |
| escalate (rare) + weekly synthesis | Opus | `claude-opus-4-8` |
| gate | none (regex `signal_detector`) | — |
| dedup embeddings | existing nano-graphrag KB embedder | (KB default) |

## Edge Cases

| scenario | expected behavior |
|---|---|
| transcript is itself a `/reflect` run | gate drops at enqueue (reflect-on-reflect detector) — never reaches LLM |
| same transcript enqueued twice (precompact + stop) | dedup by hash → single queue row |
| transcript already `done` in a prior run | drainer skips (processed-set) |
| run exceeds 8 turns or 180s | claude -p hard-stopped; entry retried with bumped counter; poison after N |
| run completes but reports >2M tokens | transcript poisoned so a retry never repeats the spend |
| concurrent drain spawns | `flock` serializes; daily cap holds |
| GraphRAG graphml corrupt (`not well-formed`) | reindex batch validates + repairs (strip doubled close-tag); logged, never escalated into reflect loop |
| LOW-only signal transcript | reflected (decision #5 = any signal) |
| clean success, no signal | gate drops → $0 |
| extraction misses a real lesson (straight-to-default risk) | surfaces in `reflect cost` low-output runs; manual re-reflect path retained |

## Risks & Mitigations

| risk | impact | likelihood | mitigation |
|---|---|---|---|
| Cascade extraction misses lessons (no A/B) | Med | Med | telemetry flags zero-output runs; keep manual `/reflect` re-run; weekly synthesis backstop |
| `signal_detector` LOW tier too noisy → over-reflect | Low | Med | cost telemetry; tighten `REFLECT_GATE_MIN_SIGNAL` later |
| JSONL→sqlite migration loses queued work | Med | Low | re-gate migration reads old JSONL, archives it, never deletes |
| launchd plist misconfig → no drains | Low | Low | `reflect cost` shows zero-activity; doctor check |
| Retiring surfacer removes a capture path some sessions relied on | Low | Low | bg drainer covers all queued transcripts; surfacer added no unique capture |
| graphml repair corrupts a valid graph | High | Low | repair only on parse-failure; back up before repair; validate after |

## Implementation Notes

### Priority order
```
 1. W1 circuit breaker      ┐ ship together (low risk, additive guards)
 2. W2 skip-gate + dedup    ┤  ← stops the bleeding immediately
 3. W3 observability        ┘  ← proves the wins
 4. W4 cascade (default-on) ── quality (behavior change)
 5. W5 structural rebuild   ── sqlite queue, launchd, reindex split, neutral cwd
```

### Technical debt accepted
- Straight-to-default cascade with no A/B harness — accepted for speed; telemetry is the safety net.
- Token-budget cap is post-hoc (poison), not mid-run kill — `claude -p` only reports usage at completion; turns+wall-clock are the mid-run hard stops.

## Open Questions

- [ ] Weekly synthesis cadence/day (default: Sunday 03:00 local) — confirm or adjust.
- [ ] launchd drain interval (default: 10 min) — confirm.
- [ ] Vector dedup threshold (default cos > 0.85) — tune after telemetry.

---

*Generated through systematic interview of `cost-rearchitecture-plan.md`. Eight decisions locked; ready for `/implement` on approval.*
