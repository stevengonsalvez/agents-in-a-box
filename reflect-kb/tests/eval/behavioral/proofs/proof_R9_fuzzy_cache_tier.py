# ABOUTME: Behavioral proof for R9 — the fuzzy (Jaccard) cache tier serves a near-miss query
# ABOUTME: from a previously-cached query's payload, gated by RECALL_FUZZY_CACHE / RECALL_FUZZY_THRESHOLD.
"""R9 fuzzy cache tier proof.

Invariant (the heart of R9 — ``fuzzy_read_cache`` in recall.py): when the
exact per-query cache key MISSES, recall.py falls back to a Tier-1 fuzzy
lookup that scans a sidecar token-set index (``recall_cache/index.json``) and
reuses a PRIOR query's cached payload when the stopword-filtered token sets
have Jaccard similarity ``>= RECALL_FUZZY_THRESHOLD`` (default 0.85), provided
the prior entry shares the same cache version / mode / limit and its payload is
still TTL/KB-mtime valid. The tier is gated by ``RECALL_FUZZY_CACHE``.

The exact tier (``cache_path`` digest) only hits on a BYTE-IDENTICAL query, so
a re-worded variant ("how should I configure postgres connection pool sizing"
vs "configure the postgres connection pool sizing") would normally pay full
retrieval again. R9 ports ByteRover's query-executor Tier 0/1 shape so that
near-identical re-asks reuse the prior payload instead.

Three arms, in tension, decide the invariant — and NONE of them can be decided
by an LLM, because the seeds, the documented env knobs, and the deterministic
``_content_terms`` tokenizer + Jaccard formula fully determine each outcome:

  A. FUZZY HIT (knob ON). Prime the cache with a BASE query (full retrieval,
     which also registers its token set in the fuzzy index). Then ask a VARIANT
     whose raw string differs (so the exact-hash tier MISSES — ``cache_path``
     keys on the literal query) but whose ``_content_terms`` token set is
     IDENTICAL to the base's (Jaccard = 1.0 >= 0.85). The variant must be
     answered by the fuzzy tier — observed as ``cache_tier == "fuzzy"`` in
     recall.py's own ``recall_log.jsonl`` (the field log_recall writes; it is
     not in the JSON envelope, so the log is the only honest observable).

  B. KNOB OFF kills the hit (proves the PORT caused arm A, not text luck). The
     SAME variant, re-asked with ``RECALL_FUZZY_CACHE=0``, must NOT fuzzy-hit:
     ``cache_tier`` is None (full retrieval) because ``fuzzy_read_cache`` short-
     circuits to None when the gate is off and ``update_cache_index`` writes
     nothing. The only thing that changed between A and B is the documented
     env flag — so the fuzzy tier, not incidental caching, drove arm A.

  C. BELOW THRESHOLD is not served (proves it is genuinely a similarity gate,
     not "any prior query"). A FAR query (Jaccard 0.0 against the cached base —
     a disjoint token set) re-asked with the knob ON must NOT fuzzy-hit:
     ``cache_tier`` is None. A near-miss within threshold hits; a miss beyond
     threshold does not.

Arm A alone could be incidental exact caching; arm B rules that out (knob
flip removes the hit) and arm C rules out "fuzzy serves everything" (a beyond-
threshold query is refused). Together they pin R9's real behavior.

PORT: R9
"""
from __future__ import annotations

import json
import os
import subprocess
import time
from pathlib import Path

# This proof drives the SAME real engine every other behavioral proof does (the
# recall.py the conftest fixture resolves) but WITH the cache enabled — the
# fixture's kb.recall() always passes --no-cache, which bypasses both cache
# tiers, so R9 can only be exercised through a direct call. conftest is loaded
# by pytest as a plugin (not an importable module from a proof), so we resolve
# recall.py the same way conftest does rather than importing from it.
_HERE = Path(__file__).parent  # reflect-kb/tests/eval/behavioral/proofs
_EVAL_ROOT = _HERE.parents[1]  # reflect-kb/tests/eval
_RECALL_CANDIDATES = [
    _EVAL_ROOT.parents[2] / "plugins" / "reflect" / "skills" / "recall" / "scripts" / "recall.py",
    _EVAL_ROOT.parents[1].parent / "plugins" / "reflect" / "skills" / "recall" / "scripts" / "recall.py",
]
RECALL_PY = next((p for p in _RECALL_CANDIDATES if p.exists()), _RECALL_CANDIDATES[0])
if not RECALL_PY.exists():
    raise RuntimeError(f"recall.py not found; tried {[str(p) for p in _RECALL_CANDIDATES]}")


def _run(cmd: list[str], env: dict, timeout: int = 300) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, capture_output=True, text=True, env=env, timeout=timeout)

# --------------------------------------------------------------------------
# One on-topic seed so the BASE query does real retrieval (non-empty payload
# that gets written to the exact cache AND registered in the fuzzy index).
# --------------------------------------------------------------------------
_SEED = dict(
    name="r9-pool-sizing",
    title="Size a Postgres connection pool below max_connections",
    category="database",
    tags=["postgres", "pooling", "connections"],
    confidence="high",
    created="2026-05-01",
    archived="2026-05-10T00:00:00",
    key_insight="Keep the pool below Postgres max_connections and set a statement timeout.",
    body=(
        "When configuring a Postgres connection pool, size it below the "
        "server's max_connections with headroom for admin and migrations, "
        "set an explicit statement timeout, and recycle idle connections so a "
        "restarted backend does not strand half-open sockets in the pool."
    ),
)

# BASE primes the cache. VARIANT has the IDENTICAL stopword-filtered token set
# {configure, connection, pool, postgres, sizing} but a different raw string,
# so the exact-hash tier misses while Jaccard(base, variant) == 1.0 >= 0.85.
# FAR shares ZERO content terms with base (Jaccard 0.0) — beyond any threshold.
_BASE_QUERY = "how should I configure postgres connection pool sizing"
_VARIANT_QUERY = "configure the postgres connection pool sizing"
_FAR_QUERY = "diagnose mysql replication lag and binlog growth"

_LIMIT = 5  # fetched_limit = max(limit*2, 10) = 10; kept identical across calls
            # so the fuzzy index's (version, mode, limit) filter matches.


def _recall_cached(
    kb, query: str, *, env: dict | None = None, want_results: bool = True
) -> dict:
    """Run recall.py WITH the cache enabled (no --no-cache) and return the
    parsed JSON envelope.

    Mirrors the fixture's kb.recall() invocation exactly EXCEPT it omits
    --no-cache, so both cache tiers are live. recall.py can emit an empty
    document (parsed as {}) — or a warm-but-empty ``count == 0`` block — on
    the engine cold-start path the first time an index is touched; that is an
    engine artifact, not R9 behavior, so up to three warm retries are allowed
    before we trust the payload (same cold-start allowance proof_R4 documents,
    hardened to also reject a spurious ``count == 0`` for queries that MUST
    retrieve the seed). ``want_results=False`` (the FAR query, which legitimately
    may match nothing) accepts any envelope carrying a ``count`` key.
    """
    cmd = [
        "python3", str(RECALL_PY), query,
        "--limit", str(_LIMIT),
        "--format", "json",
        "--confidence", "ANY",
        "--min-overlap", "0.0",
        "--max-tokens", "0",
    ]
    payload: dict = {}
    for _ in range(3):
        r = _run(cmd, kb.env(env), timeout=300)
        if r.returncode != 0:
            raise AssertionError(
                f"recall.py exited {r.returncode} for {query!r}\n"
                f"STDERR:\n{r.stderr[-1200:]}"
            )
        payload = json.loads(r.stdout or "{}")
        if "count" not in payload:
            continue  # cold-start empty document — retry warm
        if want_results and payload.get("count", 0) < 1:
            continue  # warm-but-empty cold-index block — retry
        return payload
    raise AssertionError(
        f"recall.py returned an unusable payload after retries for {query!r}: "
        f"{payload!r} — the seeded KB did not index (engine cold-start), "
        "not an R9 fault"
    )


def _freeze_cache_mtimes_forward(kb) -> None:
    """Bump every hermetic recall-cache file's mtime well into the future so a
    concurrent reindex of the REAL ``~/.learnings`` KB cannot invalidate a
    fuzzy hit mid-proof.

    recall.py's ``read_cache`` (which ``fuzzy_read_cache`` calls to validate a
    candidate payload) invalidates a cache file when
    ``kb_last_modified() > cache_mtime``, and ``kb_last_modified`` reads the
    REAL home ``~/.learnings/nano_graphrag_cache`` mtime — a path the hermetic
    fixture does NOT isolate (it only redirects GLOBAL_LEARNINGS_PATH /
    REFLECT_STATE_DIR / XDG_CACHE_HOME). Under the concurrency this suite runs
    in, another agent reindexing the shared home KB between the base prime and
    the variant read could advance that mtime past the freshly written cache
    file and silently turn the fuzzy hit into full retrieval (cache_tier None),
    flaking arm A. Pushing the cache files' mtime far ahead removes that single
    external-state dependency WITHOUT touching production code or any cache
    contents — TTL stays satisfied (future mtime is also within the 1h window
    relative to ``time.time()`` on the read side, since read_cache compares
    ``time.time() - cache_mtime > ttl`` and a future mtime makes that negative).
    Only the hermetic ``REFLECT_STATE_DIR/recall_cache`` dir is touched.
    """
    cache_dir = kb.state_dir / "recall_cache"
    if not cache_dir.exists():
        return
    future = time.time() + 3600  # +1h: ahead of any realistic concurrent write
    for f in cache_dir.glob("*.json"):
        os.utime(f, (future, future))


def _last_cache_tier(kb) -> str | None:
    """The cache_tier recall.py recorded for the MOST RECENT recall, read from
    its own recall_log.jsonl under the hermetic REFLECT_STATE_DIR.

    cache_tier is "exact" (hash hit), "fuzzy" (Jaccard match over a prior
    query), or None (full retrieval). It is written by log_recall and is NOT
    surfaced in the JSON envelope, so the log is the only honest observable of
    WHICH tier answered."""
    log = kb.state_dir / "recall_log.jsonl"
    assert log.exists(), (
        f"expected recall.py to write {log} once the cache path ran; "
        "missing log means recall ran with --no-cache or never logged"
    )
    lines = [ln for ln in log.read_text().splitlines() if ln.strip()]
    assert lines, f"recall_log.jsonl is empty at {log}"
    return json.loads(lines[-1]).get("cache_tier")


def test_R9_fuzzy_tier_serves_near_miss_and_only_within_threshold(behavioral_kb):
    """R9: a near-miss variant (Jaccard >= threshold) of a cached query is
    served by the fuzzy tier with RECALL_FUZZY_CACHE on; the same variant is
    NOT fuzzy-served with the knob off; and a beyond-threshold query is never
    fuzzy-served."""
    kb = behavioral_kb
    kb.seed([_SEED])

    # ---- Prime: BASE query does full retrieval, writes the exact-cache ----
    # payload AND registers its token set in the fuzzy index.
    base = _recall_cached(kb, _BASE_QUERY)
    assert base["count"] >= 1, (
        "the base query must retrieve the seeded learning so there is a cached "
        f"payload for the fuzzy tier to reuse; got count={base['count']}"
    )
    base_tier = _last_cache_tier(kb)
    assert base_tier is None, (
        "the FIRST time the base query runs it must be full retrieval "
        f"(cache_tier None), not a cache hit; got {base_tier!r}"
    )

    # Pin the just-written base cache file's mtime ahead of any concurrent
    # reindex of the shared home KB, so the fuzzy read in arm A validates the
    # payload (read_cache's KB-mtime check) deterministically. This is the only
    # external-state dependency in the proof and the only thing that could flip
    # arm A's fuzzy hit to None under concurrency; see helper docstring.
    _freeze_cache_mtimes_forward(kb)

    # ---- Arm A: VARIANT (same token set, different raw string) — knob ON. ----
    # Exact-hash tier misses (different literal query); fuzzy tier hits at
    # Jaccard 1.0 >= 0.85.
    variant_on = _recall_cached(kb, _VARIANT_QUERY)
    tier_on = _last_cache_tier(kb)
    assert tier_on == "fuzzy", (
        "with RECALL_FUZZY_CACHE on, the re-worded variant (token set identical "
        "to the cached base, Jaccard 1.0) must be served by the FUZZY tier — "
        f"exact-hash misses on the differing raw string; got cache_tier={tier_on!r}. "
        "If this is None, the fuzzy tier did not fire; if 'exact', the queries "
        "were not actually distinct strings."
    )
    # The fuzzy hit reused the base's payload, so the same learning comes back.
    assert variant_on["count"] >= 1 and any(
        r.get("id") == _SEED["name"] for r in variant_on["results"]
    ), (
        "the fuzzy hit must reuse the base payload (the seeded learning), got "
        f"ids {[r.get('id') for r in variant_on['results']]}"
    )

    # ---- Arm B: SAME variant, knob OFF — the fuzzy tier must not fire. ----
    variant_off = _recall_cached(
        kb, _VARIANT_QUERY, env={"RECALL_FUZZY_CACHE": "0"}
    )
    tier_off = _last_cache_tier(kb)
    assert tier_off is None, (
        "with RECALL_FUZZY_CACHE=0 the SAME variant must NOT fuzzy-hit — "
        f"fuzzy_read_cache short-circuits to None; got cache_tier={tier_off!r}. "
        "Arm B is what proves the PORT (the fuzzy gate), not incidental caching, "
        "drove arm A: the only thing that changed is the documented env flag."
    )
    assert variant_off["count"] >= 1  # full retrieval still returns the seed

    # ---- Arm C: FAR query (Jaccard 0.0), knob ON — below threshold, no hit. ----
    # want_results=False: the FAR query is about a DIFFERENT topic and may
    # legitimately retrieve nothing through the OOD gate; what arm C asserts is
    # the ABSENCE of a fuzzy hit, not the presence of results.
    far = _recall_cached(kb, _FAR_QUERY, want_results=False)
    tier_far = _last_cache_tier(kb)
    assert tier_far is None, (
        "a query whose token set is disjoint from the cached base (Jaccard 0.0, "
        "well below the 0.85 threshold) must NOT be fuzzy-served — proving R9 is "
        f"a genuine similarity gate, not 'serve any prior query'; got "
        f"cache_tier={tier_far!r}."
    )
