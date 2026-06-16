# ABOUTME: Behavioral proof for R3 — the MMR diversity step (post-rerank) de-clusters
# ABOUTME: near-duplicate candidates so a diverse-but-relevant hit reaches the top-k.
"""R3 MMR diversity proof.

Invariant (the heart of R3 — ``mmr_select`` in recall.py, applied AFTER the
rerank to pick the final top-k): when several near-DUPLICATE learnings score
high on raw relevance, plain top-k slicing (``learnings[:limit]``) lets the
redundant cluster members occupy the leading slots ahead of a DISTINCT learning
that is also relevant to the query. MMR replaces that slice with

    pick = argmax( λ·rel(d,q) − (1−λ)·max_{s∈selected} cos(d, s) )

so once ONE cluster member is selected, its near-twins are penalized by their
high cosine similarity to the already-picked member, the redundant twins are
DEMOTED, and the diverse candidate is PROMOTED past them. λ=0.7 by default (env
``RECALL_MMR_LAMBDA``); the whole step is gated by ``RECALL_MMR`` / ``--no-mmr``.

The proof is a KNOB-ON vs KNOB-OFF differential on the SAME seeds, so the
diversity is provably caused by the PORT and not by incidental ranking:

  Seeds: a CLUSTER of four near-identical learnings, all about the SAME narrow
  action ("rotate a leaked AWS IAM access key"), with bodies that differ only
  in a trailing clause (so the engine keeps them as four distinct chunks rather
  than collapsing to one, yet they embed almost on top of each other — pairwise
  cosine ≈ 1, the similarity MMR's diversity penalty reads). Plus one
  DISTINCT-but-relevant learning about a different facet of the same broad
  subject ("purge a leaked secret from git history with BFG"). The raw rerank
  ranks the distinct hit BEHIND multiple redundant cluster members.

  Knob OFF (``RECALL_MMR=0`` / ``--no-mmr``): the final selection is exactly
  ``learnings[:limit]`` — the raw rerank order. Multiple near-identical cluster
  members sit AHEAD of the distinct hit (measured: cluster, cluster, distinct).

  Knob ON (default, λ=0.7): MMR de-clusters — after the first cluster member is
  picked, the cosine penalty demotes its redundant twins, so the distinct hit is
  PROMOTED to the very next slot, ahead of the twins that preceded it with MMR
  off (measured: cluster, distinct, …). Strictly FEWER cluster members precede
  the distinct hit than with the knob off.

The MEASURABLE differential — the distinct hit ranks strictly HIGHER, and
strictly FEWER redundant cluster members precede it, with the knob ON than OFF,
on byte-identical seeds — is produced solely by toggling the documented MMR
gate. That isolates the PORT as the cause; nothing else changes between the two
runs.

No LLM participates in either assertion: the seeds (their near-duplicate vs
distinct bodies), the engine's deterministic mpnet embeddings, and the
documented ``RECALL_MMR`` / ``--no-mmr`` knob fully determine the outcome.

PORT: R3
"""
from __future__ import annotations

# --------------------------------------------------------------------------
# Cluster: four near-identical learnings about ONE narrow action. Each body is
# the same sentence + one distinct trailing clause, so:
#   * the files are not byte-identical → the engine keeps four chunks (no dedup
#     collapse to a single result), and
#   * they embed almost on top of each other → pairwise cosine ≈ 1, which is the
#     similarity MMR's diversity penalty reads.
# The cluster matches the query's exact phrasing hardest, so on raw rerank order
# these four sit above the distinct hit and would fill any small top-k.
# --------------------------------------------------------------------------
_CLUSTER_BODY = (
    "When an AWS IAM access key is leaked, immediately rotate the key: create a "
    "new access key for the IAM user, update every service and CI secret that "
    "used the old key, then deactivate and delete the compromised key in IAM so "
    "the leaked credential can no longer authenticate. "
)

_CLUSTER = [
    dict(
        name="r3-rotate-key-a",
        title="Rotate a leaked AWS IAM access key immediately",
        category="security",
        tags=["aws", "iam", "credentials", "rotation"],
        confidence="high",
        created="2026-05-01",
        archived="2026-05-10T00:00:00",
        key_insight="Rotate then delete a leaked AWS IAM access key.",
        body=_CLUSTER_BODY + "Confirm CloudTrail shows no further use of the old key afterward.",
    ),
    dict(
        name="r3-rotate-key-b",
        title="Rotate a leaked AWS IAM access key without downtime",
        category="security",
        tags=["aws", "iam", "credentials", "rotation"],
        confidence="high",
        created="2026-05-02",
        archived="2026-05-11T00:00:00",
        key_insight="Rotate then delete a leaked AWS IAM access key.",
        body=_CLUSTER_BODY + "Stagger the cutover so running workloads pick up the new key first.",
    ),
    dict(
        name="r3-rotate-key-c",
        title="Rotate a leaked AWS IAM access key across environments",
        category="security",
        tags=["aws", "iam", "credentials", "rotation"],
        confidence="high",
        created="2026-05-03",
        archived="2026-05-12T00:00:00",
        key_insight="Rotate then delete a leaked AWS IAM access key.",
        body=_CLUSTER_BODY + "Repeat the rotation for staging and production keys separately.",
    ),
    dict(
        name="r3-rotate-key-d",
        title="Rotate a leaked AWS IAM access key and audit usage",
        category="security",
        tags=["aws", "iam", "credentials", "rotation"],
        confidence="high",
        created="2026-05-04",
        archived="2026-05-13T00:00:00",
        key_insight="Rotate then delete a leaked AWS IAM access key.",
        body=_CLUSTER_BODY + "Audit which principals assumed roles with the old key before deleting it.",
    ),
]

# --------------------------------------------------------------------------
# Distinct-but-relevant: a DIFFERENT facet of the same broad subject (responding
# to a leaked credential), about purging the secret from git history rather than
# rotating an IAM key. Relevant to the broad query, but lexically further from
# the cluster's exact phrasing — so on raw rerank order it sits BELOW the four
# near-twins and only an explicit diversity step pulls it into a small top-k.
# --------------------------------------------------------------------------
_DISTINCT = dict(
    name="r3-purge-git-secret",
    title="Purge a leaked secret from git history with BFG",
    category="security",
    tags=["git", "secrets", "credentials", "history"],
    confidence="high",
    created="2026-05-05",
    archived="2026-05-14T00:00:00",
    key_insight="Scrub a committed secret out of every git revision, then force-push.",
    body=(
        "When a secret is leaked by being committed to a git repository, "
        "rotating the credential is not enough: scrub it from history. Use BFG "
        "Repo-Cleaner or git filter-repo to remove the secret from every past "
        "revision, force-push the rewritten history, and have collaborators "
        "re-clone so the leaked value is gone from the repository entirely."
    ),
)

# Broad query: 'leaked credential' covers BOTH the rotate-the-key cluster and
# the purge-it-from-git distinct hit. The cluster matches harder lexically, so
# without diversity it monopolizes a small top-k.
_QUERY = "what should I do when a credential is leaked"
# limit=3: the raw rerank places the distinct hit BEHIND two cluster members, so
# a top-3 window holds {cluster, cluster, distinct} with MMR off. MMR's
# de-clustering then promotes the distinct hit ahead of a redundant twin.
_LIMIT = 3


def _cluster_members_before(ids: list[str], target: str, cluster: set[str]) -> int:
    """How many near-duplicate cluster members rank ahead of `target`.

    `target` must be present; we assert that separately so a missing distinct
    hit fails with a clear message instead of a silent 0."""
    assert target in ids, f"expected {target!r} in results, got {ids}"
    cut = ids.index(target)
    return sum(1 for i in ids[:cut] if i in cluster)


def _recall_ids_stable(kb, query: str, attempts: int = 4, **flags) -> list[str]:
    """recall_ids with a transient-empty retry.

    An EMPTY result set is never a valid steady state for these seeds: five
    high-relevance, high-confidence learnings are indexed and the call passes
    ``--min-overlap 0`` (so R7's OOD gate cannot fire) and ``confidence ANY``
    (so the confidence filter cannot drop them). The only way recall returns
    ``[]`` is the documented booster contract in recall.py: every search arm
    "returns [] on any failure and fusion still works" — and the *primary*
    `reflect search` subprocess carries a HARD-CODED 60s timeout (recall.py
    ~L2503, not env-tunable). On a cold model load under load (e.g. a second
    pytest invocation right after the first, or concurrent agents) that one
    subprocess can exceed 60s; with the hermetic fixture's empty QMD index the
    boosters can't backfill, so recall.py returns RecallResult([], …).

    That is a transient INFRA timeout, not an MMR or seed-design outcome — the
    ranking, once results arrive, is byte-deterministic (fixed mpnet weights,
    6-decimal-rounded embeddings, deterministic CE). So we retry the recall on
    an empty set only, and let any NON-empty result through untouched. This
    hardens the harness without weakening a single behavioral assertion: every
    differential below still runs on whatever real ranking recall returns.
    """
    last: list[str] = []
    for _ in range(attempts):
        last = kb.recall_ids(query, **flags)
        if last:
            return last
    return last  # exhausted retries: return the empty set so the caller asserts


def test_R3_mmr_declusters_near_duplicates_into_topk(behavioral_kb):
    """R3: on byte-identical seeds, MMR de-clusters the four near-identical
    rotate-key learnings so the DISTINCT purge-from-git learning is PROMOTED past
    a redundant twin; turning the MMR knob OFF (--no-mmr → plain ``learnings[:k]``
    slice) leaves more redundant cluster members ahead of it.

    Two measurable differentials, both caused solely by the documented knob:
      1. the distinct hit ranks STRICTLY HIGHER with MMR ON than OFF, and
      2. STRICTLY FEWER near-duplicate cluster members precede it with MMR ON.
    Nothing but the MMR gate changes between the two runs, isolating the PORT as
    the cause."""
    kb = behavioral_kb
    kb.seed(_CLUSTER + [_DISTINCT])

    cluster_names = {c["name"] for c in _CLUSTER}
    distinct = _DISTINCT["name"]

    # ---- Knob OFF: --no-mmr → final selection is the raw rerank order, ----
    # ``learnings[:limit]``. Redundant cluster members lead the block.
    # _recall_ids_stable retries ONLY on a transient empty fetch (cold-model
    # 60s primary-search timeout under load); a non-empty ranking is asserted
    # on as-is, so the behavioral arms below are untouched by the retry.
    ids_off = _recall_ids_stable(kb, _QUERY, limit=_LIMIT, no_mmr=True)
    assert len(ids_off) == _LIMIT, (
        f"expected a full top-{_LIMIT} block from {len(_CLUSTER) + 1} seeds with "
        f"MMR off, got {ids_off}"
    )
    assert distinct in ids_off, (
        "seed-design check: the distinct hit must already be a top-3 candidate "
        "with MMR OFF so the ON-run's promotion is a re-ordering of the SAME "
        f"window (not a relevance fluke); got {ids_off}"
    )
    off_before = _cluster_members_before(ids_off, distinct, cluster_names)
    off_rank = ids_off.index(distinct)
    # Baseline must actually be cluster-led: at least two redundant near-twins
    # sit ahead of the distinct hit, or there is nothing for MMR to de-cluster.
    assert off_before >= 2, (
        "baseline check: with MMR OFF at least two near-duplicate cluster members "
        f"must precede the distinct hit (got {off_before}; order {ids_off}). If "
        "the rerank already surfaced the distinct hit first, the cluster is not "
        "dominating and the knob-on arm would prove nothing."
    )

    # ---- Knob ON (default RECALL_MMR=1, λ=0.7): MMR de-clusters — the cosine ----
    # penalty against the already-picked twin demotes the redundant near-twins
    # and promotes the distinct hit ahead of them.
    ids_on = _recall_ids_stable(kb, _QUERY, limit=_LIMIT)
    assert len(ids_on) == _LIMIT, (
        f"expected a full top-{_LIMIT} block with MMR on, got {ids_on}"
    )
    assert distinct in ids_on, (
        "with MMR ON the diversity step must keep the DISTINCT purge-from-git "
        f"learning in the top-{_LIMIT}; got {ids_on}"
    )
    on_before = _cluster_members_before(ids_on, distinct, cluster_names)
    on_rank = ids_on.index(distinct)

    # Differential 1: STRICTLY FEWER redundant cluster members precede the
    # distinct hit with MMR ON than OFF — the de-clustering effect.
    assert on_before < off_before, (
        "MMR must de-cluster: STRICTLY FEWER near-duplicate cluster members may "
        f"precede the distinct hit with the knob ON ({on_before}) than OFF "
        f"({off_before}). ON order {ids_on}; OFF order {ids_off}. The cosine "
        "penalty (1−λ)·max_sim against an already-selected near-twin (pairwise "
        "cosine ≈ 1) is what demotes the redundant twins."
    )
    # Differential 2: the distinct hit ranks strictly higher (smaller index) with
    # MMR ON than OFF — it was PROMOTED by the diversity step, not by chance.
    assert on_rank < off_rank, (
        "MMR must PROMOTE the distinct hit: its rank with the knob ON "
        f"({on_rank}) must be strictly ahead of its rank with the knob OFF "
        f"({off_rank}). ON order {ids_on}; OFF order {ids_off}. Same seeds, only "
        "the MMR gate flipped — so the promotion is the PORT, not text luck."
    )
