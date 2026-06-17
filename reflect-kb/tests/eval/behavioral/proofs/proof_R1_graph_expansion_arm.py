# ABOUTME: Behavioral proof for R1 — the entity-graph expansion retrieval arm.
# ABOUTME: Arm ON surfaces an extra entity-neighborhood candidate; RECALL_GRAPH_ARM=0 removes it.
"""R1 graph-expansion arm proof.

Invariant: recall.py runs a THIRD parallel retrieval arm that queries the engine
in `--mode local` (nano-graphrag entity-neighborhood expansion) and fuses its
result into the RRF ranking alongside the vector and BM25 arms. That arm
contributes a candidate the vector/BM25 arms do not produce on their own, so
turning it on yields STRICTLY MORE fused results than the same query with the
arm disabled via RECALL_GRAPH_ARM=0.

Why an entity-linked corpus makes the arm observable: both seeds declare entity
sidecars and a relationship, so reindex builds a real entity graph. The
`--mode local` arm returns the entity-neighborhood context as an extra fused
candidate (its per-chunk id resolves to "?" because nano-graphrag's local mode
emits CSV community/entity/relationship/source blocks rather than a single
frontmatter chunk — see conftest's note on local-mode ids). The observable
consequence is the candidate's *presence*, not its id: with the arm on the
ranking carries the graph candidate between the two vector hits; with the arm
off it is gone.

Determinism: the seeds + the RECALL_GRAPH_ARM flag fully determine the count
delta — no LLM participates in the assertion (`only_need_context=True`, so the
engine never synthesizes). Verified reproducible across repeated runs.

Falsifiability: were the graph arm absent (the pre-R1 state, or a regression
that drops the third ThreadPoolExecutor submission / stops fusing
`entity_results`), the ON and OFF counts would be EQUAL and this proof fails.

Acceptance criteria covered:
  - recall returns a hit not present in the vector/BM25 arms when docs share
    entities with the top hit (ON count > OFF count).
  - graceful degrade: a second phase disables the arm and recall still returns
    the vector/BM25 hits (no crash, non-empty), proving the arm is a booster.

PORT: R1
"""
from __future__ import annotations

# Two auth learnings wired into one entity graph. `r1-pkce-flow` is the lexical/
# semantic match for the query below; `r1-token-rotation` shares the AuthService
# entity with it. The sidecars + relationships give reindex a real graph for the
# `--mode local` arm to walk.
_SEEDS = [
    dict(
        name="r1-pkce-flow",
        title="OAuth PKCE protects the mobile authorization code flow",
        category="auth",
        tags=["oauth", "pkce", "mobile"],
        confidence="high",
        created="2026-02-01",
        key_insight="Use PKCE for public OAuth clients to defeat code interception.",
        body="Mobile apps using the OAuth authorization code flow must add PKCE so "
             "an intercepted code cannot be exchanged.",
        entities=[
            ("OAuth PKCE", "concept", "Proof Key for Code Exchange extension"),
            ("AuthService", "component", "Service handling token exchange"),
        ],
        rels=[
            ("OAuth PKCE", "AuthService", "secures",
             "PKCE secures the token exchange in AuthService", 9),
        ],
    ),
    dict(
        name="r1-token-rotation",
        title="Rotate refresh tokens on every exchange in AuthService",
        category="auth",
        tags=["refresh-token", "rotation"],
        confidence="high",
        created="2026-02-02",
        key_insight="Rotate the refresh token at each use and revoke the predecessor.",
        body="AuthService should issue a fresh refresh token on every exchange and "
             "revoke the old one to limit replay windows.",
        entities=[
            ("AuthService", "component", "Service handling token exchange"),
            ("RefreshToken", "concept", "Long-lived credential"),
        ],
        rels=[
            ("AuthService", "RefreshToken", "manages",
             "AuthService manages refresh token lifecycle", 8),
        ],
    ),
]

# Matches r1-pkce-flow lexically/semantically; the graph arm additionally walks
# the entity neighborhood and fuses its context as an extra candidate.
QUERY = "how do I stop an intercepted authorization code from being exchanged on mobile"

# nano-graphrag's local mode emits CSV blocks, so the graph candidate's parsed
# frontmatter id resolves to "?" (it is not a single per-doc chunk). The proof
# asserts on its PRESENCE, not the id text.
GRAPH_CANDIDATE_ID = "?"


def test_R1_graph_expansion_arm(behavioral_kb):
    kb = behavioral_kb
    kb.seed(_SEEDS)

    # ---- Phase 1: arm ON (default RECALL_GRAPH_ARM=1) ----
    on = kb.recall(QUERY, limit=10)
    on_ids = [r.get("id") for r in on.get("results", [])]

    # ---- Phase 2: arm OFF (RECALL_GRAPH_ARM=0) — vector + BM25 only ----
    off = kb.recall(QUERY, limit=10, env={"RECALL_GRAPH_ARM": "0"})
    off_ids = [r.get("id") for r in off.get("results", [])]

    # The graph arm is a booster, not a blocker: disabling it must still serve
    # the vector/BM25 hits (graceful-degrade acceptance criterion).
    assert off["count"] >= 1, (
        f"arm-off recall returned nothing — graph arm must be a booster, not a "
        f"gate. payload={off}"
    )
    assert "r1-pkce-flow" in off_ids, (
        f"expected the vector/BM25 hit r1-pkce-flow with the arm off, got {off_ids}"
    )

    # Core R1 invariant: the entity-graph arm fuses an ADDITIONAL candidate the
    # vector/BM25 arms do not produce, so ON yields strictly more results.
    assert on["count"] > off["count"], (
        f"expected the graph arm to add a fused candidate (ON > OFF), but got "
        f"ON count={on['count']} ids={on_ids} vs OFF count={off['count']} ids={off_ids}. "
        f"Equal counts mean the third (--mode local) arm never reached RRF fusion."
    )

    # The extra candidate is the entity-neighborhood graph context, present only
    # when the arm is on.
    assert GRAPH_CANDIDATE_ID in on_ids, (
        f"expected the graph-arm candidate (id {GRAPH_CANDIDATE_ID!r}) in the "
        f"arm-on ranking, got {on_ids}"
    )
    assert GRAPH_CANDIDATE_ID not in off_ids, (
        f"the graph-arm candidate leaked into the arm-OFF ranking, so the count "
        f"delta is not attributable to the graph arm. off_ids={off_ids}"
    )
