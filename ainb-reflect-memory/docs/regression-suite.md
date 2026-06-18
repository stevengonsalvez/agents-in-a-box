# Reflect regression suite — proving "nothing changed"

Two things must stay provably stable as the Postgres backend lands and reflect's
4.1.0 "57 ports" evolve:

1. **The backend swap changed nothing** — the shared-Postgres nano-graphrag arm
   returns the same evidence the local arm always did.
2. **The 57 ported lookup features still behave** — none drifted.

This suite covers (1) deterministically and now; (2) is covered by the existing
57 behavioral proofs plus the manifest below, with a full-stack golden tier for
absolute ranking stability.

---

## The 13 lookup types

| # | Lookup | Layer | Backend-coupled? | Source |
|---|--------|-------|------------------|--------|
| 1 | vector / semantic (naive) | nano-graphrag | **yes** (hnswlib → pgvector) | `graph_engine.py` |
| 2 | graph-local (entity neighborhood) | nano-graphrag | **yes** | `graph_engine.py` |
| 3 | graph-global (community reports) | nano-graphrag | **yes** | `graph_engine.py` |
| 4 | BM25 / lexical (QMD) | QMD | no (own `index.sqlite`) | `recall.py` (qmd) |
| 5 | typed-link graph (R1) | nano-graphrag + recall | **yes** (reads graphml/PG graph) | `graph_links.py` |
| 6 | cross-encoder rerank (R2) | recall | no | `cross_encoder.py` |
| 7 | embed + MMR diversity (R3) | recall | no (own embed call) | `learnings_cli.py embed` |
| 8 | temporal (R5/R6) | recall | no | plugin `recall.py` |
| 9 | entity / alias lookup | nano-graphrag | **yes** | `entity_store.py` |
| 10 | corpus saved-filter (M7) | recall | no (frontmatter scan) | `corpus.py` |
| 11 | RRF fusion + recency/confidence/tag rerank | recall | no | `recall.py` |
| 12 | staged 3-layer recall (M1) | recall | no | plugin `recall_stages.py` |
| 13 | per-project sharding / global scope (R15/R16) | recall | no | plugin `recall.py` |

**Backend-coupled lookups (1, 2, 3, 5, 9)** are the only ones the Postgres swap
can affect. The rest invoke `reflect search` as a subprocess and are
backend-agnostic — pinned by `tests/test_recall_backend_independence.py`.

---

## The 57 ports (reflect 4.1.0)

Source of truth: the companion gist (Hindsight · ByteRover · agentmemory ·
claude-mem · reflect, 57 ports) + GitHub Discussion #227. Each port ships an
**Acceptance** block — those are the regression cases.

| Wave | Ports | Theme |
|------|-------|-------|
| 1 | 6 | freebies / wire-an-asset (incl. **R1 graph-arm** — the only backend-coupled port) |
| 2 | 17 | ML upgrades (R2 rerank, R3 MMR, M1 staged, …) |
| 3 | 22 | storage structuring (S/A/M/O series) |
| 4 | 12 | lifecycle / maintenance (C series, M7 corpus, …) |

Families: `R*` retrieval (15) · `S*` storage · `SG*` signals · `M*` claude-mem ·
`A*` agentmemory · `O*` open-domain · `C*` consolidation. 57 = the literal count
of proof files in `reflect-kb/tests/eval/behavioral/proofs/`.

**Blast radius of the PG backend:** of the 57, exactly **one (R1)** routes
through nano-graphrag's storage; the other 56 are recall-layer and
backend-agnostic. The change is inert unless `REFLECT_PG_DSN`/`DATABASE_URL`
**and** `REFLECT_WORKSPACE_ID` are both set.

---

## Suite tiers

### Tier A — backend parity (shipped, deterministic, no model/qmd)

`tests/nanographrag/test_backend_parity.py` — seeds an identical corpus into the
**local-default** backend and the **Postgres** backend (same pinned embedding +
canned LLM), runs naive/local/global for fixed queries, and asserts the
**evidence set is identical** on both. Also asserts the local backend writes
`.graphml` while the PG backend writes none.

> Tie-break note: the two ANN engines (NanoVectorDB vs pgvector) may order
> equal-scoring items differently. Parity is asserted at evidence-set level, not
> byte level — reflect re-ranks (RRF/MMR/cross-encoder) downstream, so tie order
> doesn't reach the user. This is expected, not a regression.

Plus `tests/test_recall_backend_independence.py` (no DB) — proves the 56
recall-layer ports never reference the backend.

### Tier B — full-stack golden (scaffold; CI / full env)

Absolute ranking stability for all 13 lookups using the **real** stack
(all-mpnet-base-v2 model + `qmd` binary + the plugin recall harness). Seeds a
fixed corpus via the real save path (`reflect add` + `reflect reindex`), runs
each lookup, snapshots top-k to a checked-in golden JSON, and fails on drift.
Encodes each port's Acceptance block as a case.

This tier needs: the reflect-kb `[graph]` env, the embedding model (~420 MB),
the `qmd` binary, and `RECALL_EVAL_BIN_DIR`. It auto-skips when those are
absent (same pattern as the 57 behavioral proofs). Run in CI:

```bash
# full-stack env required (model + qmd + graph extra)
RECALL_EVAL_BIN_DIR=... pytest reflect-kb/tests/eval/behavioral   # the 57 proofs
DATABASE_URL=... pytest -m integration ainb-reflect-memory/tests/nanographrag  # tier A
```

---

## What each tier proves

- **Tier A (now):** the Postgres backend is behavior-equivalent to local for the
  5 backend-coupled lookups, and the other 8 are structurally independent.
- **Tier B (CI):** the actual ranking of all 13 lookups + the 57 ports is stable
  release-over-release with the real model.

Together they answer "did porting 57 features + adding the PG backend change
anything?" — Tier A says the backend didn't; the 57 proofs + Tier B golden say
the ports didn't.
