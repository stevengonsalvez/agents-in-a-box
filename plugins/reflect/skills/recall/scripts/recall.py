#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "pyyaml",
# ]
# ///
"""
Reflect Recall — hybrid retrieval from the global learnings KB.

Wraps the `reflect` CLI (reflect-kb, installed via `uv tool install reflect-kb`)
as a subprocess so we inherit GraphRAG + embeddings without pulling the
nano-graphrag dep chain into this plugin.

Usage:
    recall.py <query> [--limit N] [--mode naive|local|global]
                      [--confidence HIGH|MEDIUM|LOW|ANY]
                      [--format markdown|json]
                      [--max-chars 2000]
                      [--no-cache]
                      [--cache-ttl 3600]
                      [--no-mmr] [--mmr-lambda 0.7]
                      [--field rule]

Exit codes:
    0 = success (including empty results when KB absent — see D9)
    2 = invalid args
"""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import math
import os
import random
import re
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass, field
from datetime import date, datetime, timezone
from pathlib import Path
from typing import Any

import yaml  # declared in PEP 723 header; uv run --script always installs

# R6: query-time date parsing lives in a stdlib-only sibling module. When
# recall.py runs as a script its directory is already on sys.path; library
# imports (tests, recall_stages) load recall via a path insert, so mirror
# the recall_stages.py convention to keep both paths working.
sys.path.insert(0, str(Path(__file__).resolve().parent))
from temporal_extraction import (  # noqa: E402
    TemporalRange,
    extract_temporal_constraint,
)


# --- Config --------------------------------------------------------------

DEFAULT_LIMIT = 10
DEFAULT_MODE = "naive"
DEFAULT_CACHE_TTL = 3600  # 1 hour
DEFAULT_MAX_CHARS = 2000
# Canonical CLI name (reflect-kb). Resolved via `shutil.which("reflect")` so
# we honour whatever install path `uv tool install reflect-kb` produced
# (typically ~/.local/bin/reflect). Legacy `~/.learnings/cli/learnings` is
# retained as a last-ditch fallback ONLY so recall doesn't silently break
# on machines mid-migration; new installs should never hit it.
REFLECT_CLI_NAME = "reflect"
LEGACY_LEARNINGS_CLI = Path.home() / ".learnings" / "cli" / "learnings"

# R8: multiplicative bounded boosts (Hindsight `apply_combined_scoring`
# shape). Every secondary signal — confidence, recency, tag overlap, proof
# count — is normalized to [0, 1] (0.5 = exactly neutral) and applied as
#     boost = 1 + α·(norm − 0.5)        # in [1 − α/2, 1 + α/2]
# so each signal adjusts the base relevance score by at most ±α/2. Bounded
# modifiers stop any single signal dominating: a very recent low-quality
# note can no longer out-rank an older high-quality one (the old
# exp(-age/90) recency multiplier crushed year-old notes to ~2% of their
# score; now recency is worth at most ±10%). Each α is tunable via env.
def _env_alpha(name: str, default: float) -> float:
    """R8: parse a boost α from env; clamp to [0, 2] so a typo can't flip
    the boost negative or let one signal dwarf the base score."""
    try:
        value = float(os.environ.get(name, default))
    except (TypeError, ValueError):
        return default
    return min(2.0, max(0.0, value))


CONFIDENCE_ALPHA = _env_alpha("RECALL_CONFIDENCE_ALPHA", 0.2)  # ±10%
RECENCY_ALPHA = _env_alpha("RECALL_RECENCY_ALPHA", 0.2)  # ±10% (Hindsight)
TAG_ALPHA = _env_alpha("RECALL_TAG_ALPHA", 0.2)  # ±10%
# S4: proof-count boost strength — conservative ±5% (Hindsight): evidence
# nudges ordering between near-ties without overpowering quality/recency.
PROOF_COUNT_ALPHA = _env_alpha("RECALL_PROOF_ALPHA", 0.1)
# R16: project-affinity boost strength. Learnings whose project matches the
# current session's project get bounded_boost(1.0, α) = 1 + α/2 (default
# +10%); cross-project and project-less hits sit at the neutral 0.5 norm so
# their score is EXACTLY unchanged. Soft affinity, not hard isolation —
# cross-project gems still surface, just down-ranked relative to same-project
# ties. Set to 0 (env RECALL_PROJECT_ALPHA, config
# recall.boost.project_affinity_alpha) to disable entirely. When R15
# per-project sharding lands, its shard-scoped path should pass
# current_project="" so affinity only kicks in on the global path.
PROJECT_AFFINITY_ALPHA = _env_alpha("RECALL_PROJECT_ALPHA", 0.2)
# SG3: speculative down-rank strength. Idle-triggered reflections are tagged
# 'speculative' by the drain (the session went quiet rather than ending, so
# it may resume and overturn the conclusion). Speculative-tagged learnings
# take the boost FLOOR (1 − α/2, default −10%); everything else sits at the
# neutral 0.5 norm so its score is EXACTLY unchanged. Set to 0 (env
# RECALL_SPECULATIVE_ALPHA) to disable.
SPECULATIVE_ALPHA = _env_alpha("RECALL_SPECULATIVE_ALPHA", 0.2)
SPECULATIVE_TAG = "speculative"
# R8: recency normalization — linear decay over a year, floored at 0.1
# (Hindsight reranking.py): even ancient notes keep a toehold.
RECENCY_WINDOW_DAYS = 365.0
# R8: confidence tier → [0, 1] norm. MEDIUM (and unknown) sit exactly at
# the neutral 0.5 baseline so the boost collapses to 1.0 for them.
CONFIDENCE_NORMS = {"HIGH": 1.0, "MEDIUM": 0.5, "LOW": 0.0}
# S3: display tier → canonical numeric confidence (bucket midpoints — the
# same mapping the reflect.db migration backfills: HIGH→0.9, MEDIUM→0.6,
# LOW→0.3). The float is what ranking uses; tiers are display buckets.
# Unknown tiers land mid-bucket, mirroring CONFIDENCE_NORMS' neutral 0.5.
CONFIDENCE_TIER_NUMS = {"HIGH": 0.9, "MEDIUM": 0.6, "MED": 0.6, "LOW": 0.3}
DEFAULT_CONFIDENCE_NUM = 0.6
CHUNK_SEPARATOR = "--New Chunk--"
ARCHIVE_HEADER_RE = re.compile(r"<!--\s*archived:\s*([0-9T:.+\-Z]+)\s*-->")

# R1: graph-expansion arm. The engine's `local` mode walks the entity
# neighborhood (nano-graphrag) — surfacing learnings that share entities with
# the lexical/vector hits but don't match the query text directly. Disable
# with RECALL_GRAPH_ARM=0.
GRAPH_ARM_ENABLED = os.environ.get("RECALL_GRAPH_ARM", "1") != "0"

# R2: cross-encoder rerank. After RRF fusion the top candidates are scored
# jointly with the query by a local cross-encoder (`reflect rerank`; model
# cross-encoder/ms-marco-MiniLM-L-6-v2, auto-downloaded on first use and
# cached under ~/.reflect/models/). The CE score becomes the PRIMARY sort
# key; the bounded-boost formula (R8: confidence × recency × tags × proof,
# each clamped to ±α/2) is a multiplicative modifier on top of it. Slim
# reflect builds (no
# sentence-transformers) or legacy CLIs without the subcommand silently
# degrade to formula-only ordering. Disable with RECALL_CROSS_ENCODER=0.
CROSS_ENCODER_ENABLED = os.environ.get("RECALL_CROSS_ENCODER", "1") != "0"
CE_CANDIDATES = 20  # only the top fused candidates are CE-scored (one batch)
CE_TIMEOUT = int(os.environ.get("RECALL_CE_TIMEOUT", "60"))
# Candidates beyond CE_CANDIDATES get this epsilon as their CE component:
# they sort below every scored candidate, ordered by the legacy formula
# among themselves (they were already tail-ranked by RRF).
CE_UNSCORED = 1e-6

# R3: MMR diversity. After the rerank, the final top-k is selected with
# Maximal Marginal Relevance — keep the top hit, then bias subsequent picks
# AWAY from already-selected ones by embedding similarity:
#     pick = argmax( λ·rel(d,q) − (1−λ)·max_{s∈S} sim(d,s) )
# rel(d,q) is the rerank's own score (CE × formula, recency included)
# normalized by the window max; sim(d,s) is the cosine in the SAME
# all-mpnet-base-v2 space nano-graphrag indexes with (`reflect embed`, one
# subprocess batch run concurrently with the CE rerank). Stops SessionStart
# injecting 3 near-identical learnings — the later slots go to
# complementary ones.
# λ=1.0 → pure relevance, λ=0.0 → pure diversity. Disable with --no-mmr
# (benchmarking) or RECALL_MMR=0; tune λ with --mmr-lambda or
# RECALL_MMR_LAMBDA. Slim engines / legacy CLIs without the `embed`
# subcommand silently degrade to plain top-k slicing.
MMR_ENABLED = os.environ.get("RECALL_MMR", "1") != "0"
try:
    MMR_LAMBDA = float(os.environ.get("RECALL_MMR_LAMBDA", "0.7"))
except ValueError:
    MMR_LAMBDA = 0.7
MMR_CANDIDATES = 20  # embed the same top-candidate window as the CE batch
EMBED_TIMEOUT = int(os.environ.get("RECALL_EMBED_TIMEOUT", "60"))

# R7: OOD gate. Stopword-filtered query-term coverage of the top hit; below
# the threshold the whole result set is treated as out-of-domain noise and
# suppressed. 0.0 = gate off (library callers opt in; the SessionStart hook
# passes its configured threshold).
_STOPWORDS = frozenset(
    "a an the is are was were be been do does did to of in on at for with "
    "and or not no how what when where which who why our we i you it its "
    "this that these those there here from by as into over under again "
    "still now then than can could should would may might will shall am "
    "get got use used using my your".split()
)

# SG6: negative-recall knowledge-gap tracking. An empty final result set is
# itself a signal — the KB has nothing about something an agent needed. Each
# 0-result recall appends {ts, query, normalized, session_id} to
# ~/.reflect/knowledge-gaps.jsonl; the reflect-status aggregator
# (skills/reflect-status/scripts/knowledge_gaps.py) surfaces queries that
# came up empty in >=2 distinct sessions as a curation backlog ("users keep
# asking about X with no learnings"). Disable with RECALL_GAP_LOG=0 or the
# --no-gap-log flag (the SessionStart hook passes the flag — its queries are
# synthetic cwd/branch strings, not genuine asks, and would surface as fake
# gaps every session).
GAP_LOG_ENABLED = os.environ.get("RECALL_GAP_LOG", "1") != "0"

# A4: followup-rate diagnostic (agentmemory smart-search shape). The most
# recent search per session is tracked in ~/.reflect/recent-searches.json;
# when the NEXT recall in the same session arrives within the window (default
# 30s) asking something different and returning a result set fully DISJOINT
# from the prior one, the search counts as a "followup" — the empirical
# "recall didn't satisfy the first time" signal. Every tracked search appends
# an op="recall_search" line (followup: true/false) to the engine's
# metrics.jsonl (~/.learnings/metrics.jsonl) so the followup rate is
# computable offline (`reflect_cost.py --followup`, surfaced by the
# /reflect:cost skill). Skip rules match the source: no session anchor → not
# tracked; empty result sets are retrieval failures (SG6 logs those as
# knowledge gaps), not reader failures; an identical query inside the window
# is a retry, not a followup. Disable with RECALL_FOLLOWUP=0 or the
# --no-followup flag (the SessionStart hook passes the flag — its synthetic
# cwd/branch queries chased by a genuine first ask would inflate the rate
# every session). Window tunable via RECALL_FOLLOWUP_WINDOW_SECONDS.
FOLLOWUP_ENABLED = os.environ.get("RECALL_FOLLOWUP", "1") != "0"
DEFAULT_FOLLOWUP_WINDOW_SECONDS = 30.0
# State-file pruning horizon — the on-read analog of agentmemory's hourly
# TTL sweep over its recent-searches scope: entries older than this are
# dropped on every write so the file can't grow without bound.
FOLLOWUP_STATE_MAX_AGE_SECONDS = 3600.0


def _followup_window_seconds() -> float:
    """A4: detection window, env-tunable, floored at 1s (source's
    Math.max(1, …) guard); unparseable values fall back to the default."""
    raw = os.environ.get("RECALL_FOLLOWUP_WINDOW_SECONDS")
    if raw is None or not raw.strip():
        return DEFAULT_FOLLOWUP_WINDOW_SECONDS
    try:
        value = float(raw)
    except (TypeError, ValueError):
        return DEFAULT_FOLLOWUP_WINDOW_SECONDS
    return max(1.0, value)

# R9: fuzzy cache tier (ByteRover query-executor Tier 0/1 shape). The exact
# per-query cache only hits on byte-identical queries; sessions re-ask slight
# variants ("how does auth work" vs "auth flow") constantly. A sidecar index
# (~/.reflect/recall_cache/index.json) records the stopword-filtered token
# set per cached fetch; when the exact-hash lookup misses, the index is
# scanned for the best Jaccard-similarity match ≥ the threshold whose cached
# payload is still TTL/KB-mtime valid — that prior result is reused instead
# of re-running the retrieval arms. Pure latency win; disable with
# RECALL_FUZZY_CACHE=0. The bead pins the threshold at 0.85 (conservative —
# ByteRover ships 0.6 for its in-memory tier); tune via
# RECALL_FUZZY_THRESHOLD.
FUZZY_CACHE_ENABLED = os.environ.get("RECALL_FUZZY_CACHE", "1") != "0"
try:
    FUZZY_CACHE_THRESHOLD = min(
        1.0, max(0.0, float(os.environ.get("RECALL_FUZZY_THRESHOLD", "0.85")))
    )
except ValueError:
    FUZZY_CACHE_THRESHOLD = 0.85
# ByteRover guard: queries with fewer than 2 meaningful tokens are too
# ambiguous to fuzzy-match ("redis" alone would alias every redis query).
FUZZY_MIN_TOKENS = 2
# On-disk analog of ByteRover's LRU maxSize: the index is capped to the
# newest entries so it can't grow without bound across sessions.
FUZZY_INDEX_MAX_ENTRIES = 200

# R6: query-time date parsing. Natural-language date phrases ("last week",
# "in march", "since 2026-01-01") are extracted from every query into a
# TemporalRange and surfaced on RecallResult / the JSON output. Extraction
# only — the R5 temporal arm consumes the range for time-aware ranking.
# Pure-regex stdlib pass, sub-millisecond; disable with RECALL_TEMPORAL=0.
TEMPORAL_ENABLED = os.environ.get("RECALL_TEMPORAL", "1") != "0"

# R5: temporal retrieval arm (Hindsight `retrieve_temporal_combined` shape).
# When R6 extraction finds a date phrase, a 4th parallel arm scans the local
# learnings corpus for notes whose timestamp falls INSIDE the parsed window
# and feeds them into the RRF fusion alongside the vector/BM25/graph arms —
# "what did we decide last week?" ranks recency explicitly instead of
# relying on the bounded ±10% recency boost alone. Date-free queries skip
# the arm entirely (zero hits, no false boost). Disable with
# RECALL_TEMPORAL_ARM=0 (extraction itself stays on for the JSON surface).
TEMPORAL_ARM_ENABLED = os.environ.get("RECALL_TEMPORAL_ARM", "1") != "0"
# Corpus-scan bound: the arm reads frontmatter for every learning file, so
# cap the walk — a runaway docs dir must never stall the recall path.
TEMPORAL_ARM_MAX_FILES = 5000

# R12: per-arm calibrated OOD floors (Hindsight per-strategy gating shape).
# The retrieval arms are NOT score-comparable — vector cosine, BM25 score, and
# graph budget live on different scales — so the single GLOBAL OOD gate (R7,
# post-fusion) mis-calibrates at least three of them: a threshold that is right
# for the dense vector arm is wrong for the lexical BM25 arm and meaningless for
# the entity-graph arm. R12 gives each arm its OWN floor, applied to that arm's
# candidates BEFORE RRF fusion, using the same cheap, backend-agnostic signal
# R7 uses (lexical query-term coverage ∈ [0,1]) — the only score available per
# candidate without normalizing across incomparable backends. R7's global gate
# is UNCHANGED and still runs after fusion; R12 is an extra, tighter pre-fusion
# layer.
#
# Runtime default is 0 (OFF) for every arm: a non-zero floor pruning candidates
# pre-fusion would change R7's observable contract (an off-topic query whose
# candidates are all pruned arrives at R7 already empty, so R7 reports
# ood_gated=False instead of True). So the floors are OPT-IN — recall.py keeps
# byte-identical pre-R12 behaviour until an operator wires them. The CALIBRATED
# defaults live in reflect_config (`recall.arm.<name>.min_score`, derived by
# `reflect calibrate-thresholds`) as the declarative surface that hooks/tools
# read and push into the environment, exactly like the R2/R16 recall knobs. The
# calibration script's CALIBRATED_FLOORS below are what it emits as suggested
# values; they are NOT applied here automatically.
_ARM_FLOOR_DEFAULTS = {
    "vector": 0.0,
    "bm25": 0.0,
    "graph": 0.0,
    "temporal": 0.0,
}

# R12: the calibrated values `reflect calibrate-thresholds` ships as defaults
# (mirrored in reflect_config `recall.arm.<name>.min_score`). These are the
# suggested floors an operator wires via RECALL_ARM_<NAME>_MIN_SCORE; the
# calibration script seeds its output from these and refines vector/bm25 from a
# live corpus sample. Kept separate from the runtime default (all-zero / off) so
# enabling R12 is an explicit, observable choice.
CALIBRATED_FLOORS = {
    "vector": 0.1,
    "bm25": 0.15,
    "graph": 0.0,
    "temporal": 0.05,
}


def _arm_floor(arm: str) -> float:
    """R12: the active min_score floor for *arm*, env-overridable.

    Defaults to 0 (OFF) — see _ARM_FLOOR_DEFAULTS — so R12 is opt-in and never
    changes pre-R12 behaviour unless wired. RECALL_ARM_<NAME>_MIN_SCORE (e.g.
    RECALL_ARM_VECTOR_MIN_SCORE) sets the arm's floor; malformed values fall
    back to the default. Clamped to [0, 1] — the floor is a fraction of
    query-term coverage, same units as R7's --min-overlap, so the two gates are
    directly comparable.
    """
    default = _ARM_FLOOR_DEFAULTS.get(arm, 0.0)
    raw = os.environ.get(f"RECALL_ARM_{arm.upper()}_MIN_SCORE")
    if raw is None:
        return default
    try:
        return min(1.0, max(0.0, float(raw)))
    except ValueError:
        return default

# R4: token-budget retrieval. Rough estimate — 1 token ≈ 4 chars — matching
# Hindsight's budget-not-top-k contract (agents think in tokens).
def _est_tokens(text: str) -> int:
    return max(1, len(text) // 4)

# M8: token-economics surfacing (claude-mem TokenCalculator shape). Every
# rendered recall block shows three numbers per learning — discovery_tokens
# (estimated cost to find this info in the wild), read_tokens (cost to
# re-read the stored note, via the same ≈4-chars/token estimator the R4
# budget uses), savings_pct — next to a type glyph drawn from the ACTIVE
# mode's learning_types (M4 ``work_emoji``; never a hardcoded table). Block
# totals roll up to the header line, and the SessionStart hook appends a
# one-line economics footer. Disable with RECALL_ECONOMICS=0.
ECONOMICS_ENABLED = os.environ.get("RECALL_ECONOMICS", "1") != "0"
# Discovery-cost fallbacks for notes that predate the write-time
# ``discovery_tokens`` frontmatter field AND whose source transcript
# (provenance.source_path) is gone: category averages keyed by
# learning_type — debugging-heavy types cost more to re-derive than a
# recorded decision. The bead's "category average" tier.
DISCOVERY_CATEGORY_AVERAGES = {
    "bug-fix": 3000,
    "anti-pattern": 2500,
    "correction": 2000,
    "pattern": 1500,
    "decision": 1200,
}
DEFAULT_DISCOVERY_TOKENS = 1500
# Glyph for a learning whose type the active mode doesn't declare. A single
# neutral placeholder, NOT a type→glyph mapping — the mapping itself always
# comes from the mode (claude-mem getWorkEmoji falls back the same way).
ECONOMICS_FALLBACK_GLYPH = "·"

# --- QMD fusion config ---------------------------------------------------
# QMD provides BM25 lexical search (fast, ~0.5s) as a complement to
# GraphRAG's vector path. Fusing the two via RRF gives hybrid lex+vec
# retrieval without changing the reflect CLI.
QMD_COLLECTION = "learnings"
QMD_DOCS_ROOT = Path.home() / ".learnings" / "documents"
QMD_PATH_RE = re.compile(r"qmd://" + re.escape(QMD_COLLECTION) + r"/(\S+?\.md)")
RRF_K = 60  # standard reciprocal-rank-fusion constant

# R15: per-project sharding (Hindsight `bank_id` partitioning + per-bank HNSW
# shape). Each project keeps its OWN nano-graphrag index under
# ~/.learnings/shards/<project>/ instead of pooling into one global KB.
# Smaller per-project corpora → faster recall and clean isolation (no
# cross-project noise in injection). recall.py defaults to the CURRENT
# project's shard; `--global` (or RECALL_GLOBAL=1) searches the pooled
# ~/.learnings KB across all projects.
#
# The engine resolves its KB from $GLOBAL_LEARNINGS_PATH (learnings_cli.py
# get_repo_path). We thread the chosen KB root to the `reflect` subprocess
# via that env var AND scope the corpus-scan arms (QMD/temporal docs root)
# to the same root, so all arms agree on which shard they read.
#
# Hard precedence (highest wins):
#   1. an EXPLICIT pre-set $GLOBAL_LEARNINGS_PATH — the eval/behavioral
#      harnesses and any isolated caller pin the KB this way; sharding must
#      never clobber it (else hermetic tests would leak into real shards).
#   2. `--global` / RECALL_GLOBAL=1 → the pooled GLOBAL_LEARNINGS_ROOT.
#   3. the current project's shard, when a project is detectable.
#   4. the pooled global KB (no project → nothing to shard by).
SHARDS_DIRNAME = "shards"
# A6: branch-aware sharding. Within a project shard, each git branch/worktree
# keeps its OWN sub-index under ``shards/<project>/branches/<branch>/`` so two
# worktrees of the same repo (agents-in-a-box LITERALLY runs
# ``/.agents-in-a-box/worktrees/by-name/...``) don't pollute each other's
# recall. The project-level shard root (``shards/<project>/``, R15's path)
# becomes the ALL-BRANCHES scope: ``--all-branches`` / RECALL_ALL_BRANCHES
# searches it, pooling every branch's learnings. Default recall scope is the
# CURRENT branch's sub-shard. Detection: ``git rev-parse --abbrev-ref HEAD``
# (worktree-aware — each worktree reports its own checked-out branch). The
# main/master branch and a detached HEAD map to NO branch shard (they fall
# back to the project-level shard) so the common single-branch case keeps
# R15's exact path and behaviour.
BRANCHES_DIRNAME = "branches"
# Branches we never carve a sub-shard for — the trunk a fresh clone sits on,
# and a detached HEAD (no branch identity). These collapse to the
# project-level shard so the no-worktree common case is byte-identical to R15.
_TRUNK_BRANCHES = frozenset({"main", "master", ""})
# RECALL_GLOBAL=1 is the env analog of the --global flag (so the SessionStart
# hook / settings.json can force global scope without a CLI edit).
RECALL_GLOBAL_ENV = os.environ.get("RECALL_GLOBAL", "").strip().lower() in (
    "1", "true", "yes", "on",
)
# A6: RECALL_ALL_BRANCHES=1 is the env analog of --all-branches (so the
# SessionStart hook / settings.json can widen scope to every branch of the
# current project without a CLI edit).
RECALL_ALL_BRANCHES_ENV = os.environ.get(
    "RECALL_ALL_BRANCHES", ""
).strip().lower() in ("1", "true", "yes", "on")


def _global_learnings_root() -> Path:
    """R15: the pooled global KB root that every shard lives beneath.

    Defaults to ``~/.learnings`` (the canonical KB the engine + ingest use).
    ``RECALL_LEARNINGS_ROOT`` overrides it so hermetic tests (and a relocated
    install) can sandbox the shard tree without touching the user's real KB.
    Resolved on each call so tests can flip the env without re-importing.
    """
    override = (os.environ.get("RECALL_LEARNINGS_ROOT") or "").strip()
    if override:
        return Path(override).expanduser()
    return Path.home() / ".learnings"


# Back-compat module constant — the default pooled root. Prefer
# _global_learnings_root() on the hot path so RECALL_LEARNINGS_ROOT is honoured.
GLOBAL_LEARNINGS_ROOT = Path.home() / ".learnings"


def _shards_root() -> Path:
    """R15: the parent dir holding every per-project shard."""
    return _global_learnings_root() / SHARDS_DIRNAME


def _sanitize_branch(raw: Any) -> str:
    """A6: normalize a git branch/worktree branch name to a filesystem-safe
    shard-dir component.

    Git allows ``/`` in branch names (``feat/auth``); a raw branch would mint
    nested dirs and let ``feat/auth`` and ``feat`` collide with a real
    ``branches/feat`` subtree. Slashes (and any path separator) collapse to
    ``__`` so each branch maps to exactly one flat shard dir. Trunk branches
    (main/master) and a missing/detached value return "" — the caller then
    falls back to the project-level shard (R15's path). Lowercased so case-
    only branch variants don't fragment the shard tree on case-insensitive
    filesystems.
    """
    if raw is None or isinstance(raw, bool):
        return ""
    text = str(raw).strip()
    if not text or text in _TRUNK_BRANCHES:
        return ""
    # Collapse every path separator + a few shell-hostile chars to "__".
    safe = re.sub(r"[/\\\s:]+", "__", text).strip("._").lower()
    # A detached HEAD reports "HEAD" from --abbrev-ref; treat it as no branch.
    if not safe or safe == "head":
        return ""
    return safe


def shard_kb_path(project: str, branch: str = "") -> Path | None:
    """R15 + A6: the shard KB dir for ``project`` (and ``branch``).

    Without a branch (R15): ``~/.learnings/shards/<project>/`` — the
    project-level shard, also the ALL-BRANCHES scope under A6.

    With a branch (A6): ``~/.learnings/shards/<project>/branches/<branch>/`` —
    an isolated per-branch/per-worktree sub-shard so two worktrees of one repo
    don't pollute each other's recall.

    ``project`` is the normalized id (:func:`_normalize_project` output);
    ``branch`` is the sanitized component (:func:`_sanitize_branch` output —
    "" for trunk/detached). Returns None for an empty project so callers fall
    back to the pooled global KB rather than minting a ``shards//`` path.
    """
    if not project:
        return None
    root = _shards_root() / project
    if branch:
        return root / BRANCHES_DIRNAME / branch
    return root


def resolve_kb_root(
    scope_global: bool, all_branches: bool = False
) -> Path | None:
    """R15 + A6: pick the KB root the `reflect` subprocess + corpus arms read.

    Returns None to mean "leave $GLOBAL_LEARNINGS_PATH untouched / use the
    engine default" — i.e. an explicit pre-set override is in force, or no
    shard applies. Concrete precedence (highest wins):

      1. explicit pre-set $GLOBAL_LEARNINGS_PATH override (harness contract)
      2. global scope (``--global`` / RECALL_GLOBAL) → pooled KB, all projects
      3. current project + current branch → the per-branch sub-shard (A6)
         (UNLESS ``all_branches`` / RECALL_ALL_BRANCHES widens to the
         project-level shard, pooling every branch of the project)
      4. current project, no branch (trunk/detached) → the project shard (R15)
      5. no project context → pooled global KB

    A6: per-branch isolation only kicks in when a NON-trunk branch is
    detectable AND ``all_branches`` is off — so the no-worktree main/master
    case stays byte-identical to R15.
    """
    # (1) An explicit pre-set override always wins — never clobber a harness'
    #     hermetic KB or a caller that already chose a path.
    if (os.environ.get("GLOBAL_LEARNINGS_PATH") or "").strip():
        return None
    # (2) Global scope → the pooled KB across all projects.
    if scope_global or RECALL_GLOBAL_ENV:
        return _global_learnings_root()
    project = detect_current_project()
    # (3/4) Current project → its shard. A6: scope to the current branch's
    #       sub-shard unless --all-branches widens to the project level.
    branch = "" if (all_branches or RECALL_ALL_BRANCHES_ENV) else detect_current_branch()
    shard = shard_kb_path(project, branch)
    if shard is not None:
        return shard
    # (5) No project context → fall back to the pooled global KB.
    return _global_learnings_root()


def recall_env(
    scope_global: bool, all_branches: bool = False
) -> tuple[dict[str, str], Path | None]:
    """R15 + A6: subprocess env + KB root for the chosen scope.

    The returned env is ``os.environ`` plus ``GLOBAL_LEARNINGS_PATH`` set to
    the resolved KB root (so the `reflect` subprocess reads that shard). When
    :func:`resolve_kb_root` returns None (explicit override in force) the env
    is the untouched ``os.environ`` and the root is None — the corpus arms
    then read whatever the existing $GLOBAL_LEARNINGS_PATH points at, exactly
    as before R15. ``all_branches`` widens the per-branch scope to the
    project-level shard (A6).
    """
    root = resolve_kb_root(scope_global, all_branches)
    env = dict(os.environ)
    if root is not None:
        env["GLOBAL_LEARNINGS_PATH"] = str(root)
    return env, root


def _docs_root_for(kb_root: Path | None) -> Path:
    """R15: the documents/ dir the corpus-scan arms (QMD/temporal) read.

    ``kb_root`` is recall_env's resolved root: a shard, the pooled global, or
    None. None means "honour whatever $GLOBAL_LEARNINGS_PATH is already set
    to" — the pre-R15 behaviour fetch_temporal/fetch_qmd already implement —
    so the corpus arm and the `reflect` subprocess never disagree on shard.
    """
    if kb_root is not None:
        return kb_root / "documents"
    override = os.environ.get("GLOBAL_LEARNINGS_PATH")
    return Path(override) / "documents" if override else QMD_DOCS_ROOT


# S1: structured field projection. New learnings carry typed frontmatter
# fields (problem / root_cause / fix / rule / category / entities /
# causal_relations — the Hindsight fact_extraction schema, written by the
# drain per assets/learning_template.md). `--field rule` returns JUST that
# field per hit instead of the whole note — a large context-efficiency win
# on injection. Legacy free-form notes degrade gracefully: a missing field
# falls back to the matching prose body section (## Problem → `problem`,
# ## Solution → `fix`), then key_insight/title in the markdown rendering.
FIELD_BODY_SECTIONS = {
    "problem": "Problem",
    "fix": "Solution",
    "root_cause": "Root Cause",
}
FIELD_VALUE_MAX_CHARS = 500


def _stringify_field(raw: Any) -> str:
    """S1: render one frontmatter value as a compact single string.

    Scalars stringify; lists of scalars comma-join; anything nested (the
    causal_relations list-of-dicts) JSON-dumps — always bounded so a bloated
    field can't undo the projection's context savings. Returns "" for
    missing/empty values so callers can fall back.
    """
    if raw is None or isinstance(raw, bool):
        return ""
    if isinstance(raw, str):
        text = raw.strip().strip('"')
    elif isinstance(raw, (int, float)):
        text = str(raw)
    elif isinstance(raw, list):
        if any(isinstance(i, (dict, list)) for i in raw):
            text = json.dumps(raw, default=str)
        else:
            text = ", ".join(str(i).strip() for i in raw if str(i).strip())
    elif isinstance(raw, dict):
        text = json.dumps(raw, default=str)
    else:
        text = str(raw)
    return text.strip()[:FIELD_VALUE_MAX_CHARS]


# --- Data models ---------------------------------------------------------

@dataclass
class Learning:
    """One parsed chunk from the learnings search output."""

    chunk_text: str
    frontmatter: dict[str, Any] = field(default_factory=dict)
    archived_at: str | None = None  # ISO timestamp from the <!-- archived --> comment

    @property
    def id(self) -> str:
        return self.frontmatter.get("id") or self.frontmatter.get("name") or "?"

    @property
    def title(self) -> str:
        return (
            self.frontmatter.get("title")
            or self.frontmatter.get("name")
            or "(no title)"
        ).strip().strip('"')

    @property
    def key_insight(self) -> str:
        return (self.frontmatter.get("key_insight") or "").strip().strip('"')

    @property
    def confidence(self) -> str:
        raw = self.frontmatter.get("confidence")
        if raw is None:
            return "MEDIUM"
        # Coerce numeric confidence (instinct-style 0.0-1.0) to tier.
        # Explicit None check above so `0`/`0.0` reach this branch, not the default.
        if isinstance(raw, bool):
            # bool is a subclass of int — treat as a tier string via str().upper()
            return str(raw).upper()
        if isinstance(raw, (int, float)):
            if raw >= 0.8:
                return "HIGH"
            if raw >= 0.5:
                return "MEDIUM"
            return "LOW"
        return str(raw).upper()

    @property
    def confidence_num(self) -> float:
        """S3: continuous confidence 0–1 — the value ranking uses.

        Lookup order: explicit ``confidence_num`` frontmatter → a numeric
        ``confidence`` (instinct-style 0.0–1.0 notes) → the tier midpoint
        (HIGH→0.9, MEDIUM→0.6, LOW→0.3; unknown→0.6). The midpoint fallback
        is anchored so tier-only legacy notes rank EXACTLY as they did
        before S3 (see :func:`confidence_num_norm`). Values are clamped to
        [0, 1]; malformed values degrade to the tier path, never crash.
        """
        raw = self.frontmatter.get("confidence_num")
        if raw is None:
            legacy = self.frontmatter.get("confidence")
            if isinstance(legacy, (int, float)) and not isinstance(legacy, bool):
                raw = legacy
        if raw is not None and not isinstance(raw, bool):
            try:
                num = float(raw)
                if num == num:  # NaN guard
                    return min(1.0, max(0.0, num))
            except (ValueError, TypeError):
                pass
        return CONFIDENCE_TIER_NUMS.get(self.confidence, DEFAULT_CONFIDENCE_NUM)

    @property
    def tags(self) -> list[str]:
        raw = self.frontmatter.get("tags") or []
        if isinstance(raw, str):
            # yaml sometimes leaves unquoted lists as strings; split tolerantly
            raw = [t.strip() for t in re.split(r"[\[\],]", raw) if t.strip()]
        return [str(t).strip() for t in raw]

    @property
    def proof_count(self) -> int | None:
        """S4: evidence count from frontmatter (top-level or under provenance).

        Returns None when absent or malformed — the reranker treats None as
        a neutral baseline so legacy notes are never penalised.
        """
        raw = self.frontmatter.get("proof_count")
        if raw is None:
            provenance = self.frontmatter.get("provenance")
            if isinstance(provenance, dict):
                raw = provenance.get("proof_count")
        if raw is None or isinstance(raw, bool):
            return None
        try:
            return int(raw)
        except (ValueError, TypeError):
            return None

    @property
    def project_id(self) -> str:
        """R16: which project this learning came from, normalized for the
        affinity match. Prefers explicit `project_id`, falls back to
        `project`. Returns "" when absent — the affinity boost treats
        unknown-project learnings as neutral, never penalised.
        """
        raw = self.frontmatter.get("project_id") or self.frontmatter.get("project")
        return _normalize_project(raw)

    @property
    def learning_type(self) -> str:
        """M8: the note's taxonomy type (``learning_type`` frontmatter,
        normalized lowercase) — the key the mode's glyph mapping and the
        discovery category averages are looked up by. "" when absent."""
        raw = self.frontmatter.get("learning_type")
        if raw is None or isinstance(raw, bool):
            return ""
        return str(raw).strip().lower()

    @property
    def discovery_tokens(self) -> int:
        """M8: estimated tokens the ORIGINATING session spent discovering
        this information (claude-mem ``obs.discovery_tokens`` shape).

        Resolution order:
          1. ``discovery_tokens`` frontmatter — the integer captured at
             write time (learning_template.md declares it; the drain fills
             it from the source transcript length).
          2. Source transcript size (``provenance.source_path``) ÷ 4 — the
             re-derivation cost when the transcript still exists. stat()
             only, never a read: transcripts can be multi-MB.
          3. Category average by learning_type, then the generic default.
        Never raises; malformed values degrade down the chain.
        """
        raw = self.frontmatter.get("discovery_tokens")
        if raw is not None and not isinstance(raw, bool):
            try:
                num = int(raw)
                if num >= 0:
                    return num
            except (TypeError, ValueError):
                pass
        provenance = self.frontmatter.get("provenance")
        if isinstance(provenance, dict):
            source = provenance.get("source_path")
            if isinstance(source, str) and source.strip():
                try:
                    size = Path(source.strip()).expanduser().stat().st_size
                    if size > 0:
                        return max(1, size // 4)
                except OSError:
                    pass
        return DISCOVERY_CATEGORY_AVERAGES.get(
            self.learning_type, DEFAULT_DISCOVERY_TOKENS
        )

    @property
    def read_tokens(self) -> int:
        """M8: cost to re-read this stored learning on injection — the SAME
        ≈4-chars/token estimator the R4 token budget uses (the active
        tokenizer of this pipeline)."""
        return _est_tokens(self.chunk_text)

    @property
    def how_to_apply(self) -> str:
        """Extract the "How to apply:" paragraph from the chunk body."""
        m = re.search(
            r"\*\*How to apply:\*\*\s*\n?(.*?)(?=\n\n|\n\*\*|\Z)",
            self.chunk_text,
            re.DOTALL,
        )
        if m:
            text = m.group(1).strip()
            # Cap at one sentence / 280 chars for SessionStart brevity
            text = text.split("\n")[0]
            return text[:280]
        return ""

    def _body_section(self, heading: str) -> str:
        """S1: extract one `## <heading>` section's first paragraph from the
        chunk body — the graceful-degradation path for legacy free-form notes
        that predate structured frontmatter fields."""
        m = re.search(
            rf"^##\s+{re.escape(heading)}\s*\n+(.*?)(?=\n##\s|\Z)",
            self.chunk_text,
            re.DOTALL | re.MULTILINE,
        )
        if not m:
            return ""
        text = " ".join(m.group(1).split())
        return text[:FIELD_VALUE_MAX_CHARS]

    def field_value(self, name: str) -> str:
        """S1: one structured frontmatter field as a compact string.

        Lookup order: frontmatter[name] → the matching prose body section
        (FIELD_BODY_SECTIONS — so pre-S1 notes still answer `--field problem`
        / `--field fix` from their Problem/Solution prose) → "". Callers
        decide the final fallback ("" lets render_markdown substitute
        key_insight/title while render_json reports null honestly).
        """
        text = _stringify_field(self.frontmatter.get(name))
        if text:
            return text
        heading = FIELD_BODY_SECTIONS.get(name)
        if heading:
            return self._body_section(heading)
        return ""


@dataclass
class RecallResult:
    learnings: list[Learning]
    query: str
    mode: str
    cache_hit: bool = False
    # R9: which cache tier answered — "exact" (hash hit), "fuzzy" (Jaccard
    # match over a prior near-identical query), or None (full retrieval).
    cache_tier: str | None = None
    error: str | None = None
    ood_gated: bool = False  # R7: True when the OOD gate suppressed results
    # M1: final per-candidate rank scores keyed by _learning_key — the staged
    # pipeline (recall_stages.py reflect_index) surfaces them as compact
    # ID-only index rows. Empty on error returns.
    scores: dict[str, float] = field(default_factory=dict)
    # R6: the date range parsed out of the query ("last week", "in march",
    # "since 2026-01-01"), or None when the query carries no date phrase.
    # Populated on EVERY return path (including errors) — the R5 temporal
    # arm and downstream callers read it regardless of retrieval outcome.
    temporal: TemporalRange | None = None


# --- Helpers -------------------------------------------------------------

def find_learnings_cli() -> Path | None:
    """Locate the reflect-kb CLI. D1: subprocess wrapper.

    Resolution order:
      1. `shutil.which("reflect")` — canonical install via `uv tool install reflect-kb`.
         Resolves through $PATH so it picks up whatever the user's environment
         points at (usually ~/.local/bin/reflect).
      2. Legacy `~/.learnings/cli/learnings` — pre-migration install. Kept only
         so machines that haven't installed reflect-kb yet don't silently lose
         recall; new code paths should never hit this.

    Trust boundary: `$PATH` is only as trustworthy as the caller's environment,
    but this script only runs in the user's own session — a hostile `$PATH`
    would already compromise their shell.

    Returns None if neither is found. Caller surfaces a graceful empty result.
    """
    cli_on_path = shutil.which(REFLECT_CLI_NAME)
    if cli_on_path:
        return Path(cli_on_path)
    if LEGACY_LEARNINGS_CLI.exists() and os.access(LEGACY_LEARNINGS_CLI, os.X_OK):
        return LEGACY_LEARNINGS_CLI
    return None


CACHE_VERSION = "v8-branch"  # bump when fusion / cache-key semantics change


def _cache_scope_token(mode: str, kb_root: Path | None) -> str:
    """R15: a cache `mode` component that ALSO encodes the shard.

    The same query against two projects' shards (or the pooled global KB)
    returns different result sets, so they must not share a cache entry.
    Folding the resolved KB root into the mode component keeps cache_path /
    fuzzy_read_cache / update_cache_index unchanged while making every cache
    key shard-specific. ``kb_root`` None (explicit $GLOBAL_LEARNINGS_PATH
    override in force) reuses the bare mode — pre-R15 keys, so an
    override-driven caller's cache is unaffected.
    """
    if kb_root is None:
        return mode
    digest = hashlib.sha1(str(kb_root).encode()).hexdigest()[:8]
    return f"{mode}@{digest}"


def cache_path(query: str, mode: str, limit: int) -> Path:
    """Per-query cache file. D4: 1-hour TTL.

    Limit is part of the key so a small-limit fetch can't poison a
    subsequent large-limit read with a truncated result set. Version tag
    invalidates old caches when the fusion pipeline changes.

    `query_tags` is intentionally NOT part of the key: tags only affect
    rerank ordering (applied after cache read) and the fetched raw set
    is tag-independent, so two calls with same (query, mode, limit) but
    different tags correctly share a cached fetch.
    """
    digest = hashlib.sha1(
        f"{CACHE_VERSION}|{query}|{mode}|{limit}".encode()
    ).hexdigest()[:16]
    cache_dir = _cache_dir()
    cache_dir.mkdir(parents=True, exist_ok=True)
    return cache_dir / f"{digest}.json"


def _cache_dir() -> Path:
    """Recall cache directory under the (overridable) reflect state dir."""
    base = Path(os.environ.get("REFLECT_STATE_DIR", Path.home() / ".reflect"))
    return base / "recall_cache"


def kb_last_modified() -> float:
    """mtime of the GraphRAG cache dir — proxy for last KB write."""
    kb = Path.home() / ".learnings" / "nano_graphrag_cache"
    try:
        return kb.stat().st_mtime if kb.exists() else 0.0
    except OSError:
        return 0.0


def read_cache(path: Path, ttl: int) -> dict | None:
    if not path.exists():
        return None
    cache_mtime = path.stat().st_mtime
    # Invalidate on TTL or when KB has been written since the cache was created
    if time.time() - cache_mtime > ttl or kb_last_modified() > cache_mtime:
        return None
    try:
        return json.loads(path.read_text())
    except (json.JSONDecodeError, OSError):
        return None


def write_cache(path: Path, payload: dict) -> None:
    try:
        path.write_text(json.dumps(payload, default=str))
    except OSError as e:
        # Disk full / permission / path too long — non-fatal, but surface
        # in debug mode so silent cache-write failures don't hide real
        # issues (e.g. $HOME on a read-only volume).
        if os.environ.get("REFLECT_RECALL_DEBUG"):
            print(f"recall: cache write failed: {e}", file=sys.stderr)


# --- R9: fuzzy cache tier --------------------------------------------------

def query_token_set(query: str) -> set[str]:
    """R9: stopword-filtered token set for Jaccard matching.

    Reuses :func:`_content_terms` — the SAME tokenizer the R7 OOD gate and
    SG6 gap normalization use — so one notion of "meaningful query term"
    holds across the whole recall path. (ByteRover's tokenizeQuery keeps
    2-char tokens; _content_terms requires 3+, a slightly stricter filter.)
    """
    return _content_terms(query)


def jaccard_similarity(a: set[str], b: set[str]) -> float:
    """R9: |a ∩ b| / |a ∪ b| in [0, 1]. Two empty sets are identical (1.0);
    one empty set shares nothing (0.0) — ByteRover jaccardSimilarity shape."""
    if not a and not b:
        return 1.0
    if not a or not b:
        return 0.0
    union = len(a | b)
    return len(a & b) / union if union else 0.0


def _cache_index_path() -> Path:
    return _cache_dir() / "index.json"


def read_cache_index() -> dict[str, Any]:
    """R9: load the fuzzy-tier sidecar index ({digest: entry}). Returns {} on
    any problem — a corrupt index degrades to exact-only caching, never an
    error."""
    try:
        data = json.loads(_cache_index_path().read_text())
    except (OSError, json.JSONDecodeError):
        return {}
    return data if isinstance(data, dict) else {}


def _index_entry_age(entry: Any) -> float:
    if not isinstance(entry, dict):
        return 0.0
    try:
        return float(entry.get("stored_at", 0.0))
    except (TypeError, ValueError):
        return 0.0


def update_cache_index(
    query: str, mode: str, limit: int, cache_file: Path
) -> None:
    """R9: record this fetch's token set in the fuzzy index.

    Keyed by the cache file's digest stem so a fuzzy hit can resolve back to
    the exact payload file (whose TTL/KB-mtime validity :func:`read_cache`
    still enforces). Entries whose payload file has vanished are pruned and
    the index is capped to the newest FUZZY_INDEX_MAX_ENTRIES (the on-disk
    analog of ByteRover's LRU maxSize). Silent-fail like :func:`write_cache`.
    """
    if not FUZZY_CACHE_ENABLED:
        return
    try:
        cache_dir = _cache_dir()
        index = read_cache_index()
        index[cache_file.stem] = {
            "query": query[:200],
            "tokens": sorted(query_token_set(query)),
            "mode": mode,
            "limit": limit,
            "version": CACHE_VERSION,
            "stored_at": time.time(),
        }
        index = {
            digest: entry
            for digest, entry in index.items()
            if isinstance(entry, dict)
            and (cache_dir / f"{digest}.json").exists()
        }
        if len(index) > FUZZY_INDEX_MAX_ENTRIES:
            newest = sorted(
                index.items(), key=lambda kv: _index_entry_age(kv[1]),
                reverse=True,
            )
            index = dict(newest[:FUZZY_INDEX_MAX_ENTRIES])
        cache_dir.mkdir(parents=True, exist_ok=True)
        _cache_index_path().write_text(json.dumps(index))
    except OSError as e:
        if os.environ.get("REFLECT_RECALL_DEBUG"):
            print(f"recall: cache index write failed: {e}", file=sys.stderr)


def fuzzy_read_cache(
    query: str, mode: str, limit: int, ttl: int
) -> dict | None:
    """R9: Tier-1 fuzzy lookup — best Jaccard match over the cached token
    sets (ByteRover ``findSimilar``), tried AFTER the exact-hash read misses.

    Only entries with the same (version, mode, limit) compete — a fuzzy hit
    must be interchangeable with what the exact key would have fetched.
    Candidates are tried best-similarity-first and the first whose payload
    passes :func:`read_cache` (TTL + KB-mtime — TTL is still respected) wins;
    expired or vanished payloads are simply skipped. Returns the cached
    payload dict or None.
    """
    if not FUZZY_CACHE_ENABLED:
        return None
    tokens = query_token_set(query)
    if len(tokens) < FUZZY_MIN_TOKENS:
        return None  # too ambiguous to alias (ByteRover guard)
    candidates: list[tuple[float, str]] = []
    for digest, entry in read_cache_index().items():
        if not isinstance(entry, dict):
            continue
        if entry.get("version") != CACHE_VERSION:
            continue
        if entry.get("mode") != mode or entry.get("limit") != limit:
            continue
        raw = entry.get("tokens")
        if not isinstance(raw, list):
            continue
        sim = jaccard_similarity(tokens, {str(t) for t in raw})
        if sim >= FUZZY_CACHE_THRESHOLD:
            candidates.append((sim, str(digest)))
    candidates.sort(reverse=True)  # best similarity first; digest tiebreak
    for _sim, digest in candidates:
        payload = read_cache(_cache_dir() / f"{digest}.json", ttl)
        if payload is not None:
            return payload
    return None


def parse_frontmatter(text: str) -> tuple[dict[str, Any], str]:
    """Extract YAML frontmatter if present; return (dict, remaining_body)."""
    if not text.startswith("---"):
        return {}, text
    end = text.find("\n---", 3)
    if end == -1:
        return {}, text
    header = text[3:end].strip()
    body = text[end + 4 :].lstrip("\n")
    try:
        data = yaml.safe_load(header) or {}
        return (data if isinstance(data, dict) else {}), body
    except yaml.YAMLError:
        return {}, body


def find_qmd_cli() -> Path | None:
    """Locate the `qmd` binary. Returns None if not installed."""
    cli_on_path = shutil.which("qmd")
    return Path(cli_on_path) if cli_on_path else None


def fetch_qmd(
    query: str, limit: int, timeout: int = 10, docs_root: Path | None = None
) -> list[Learning]:
    """Fast BM25 retrieval via qmd. Complement to GraphRAG's vector path.

    Returns empty list on any failure (missing CLI, timeout, empty KB) — QMD
    is strictly a booster, never a blocker.

    R15: ``docs_root`` scopes which shard's documents back the hits (the
    parsed paths are resolved relative to it); None keeps the pre-R15 default
    (``QMD_DOCS_ROOT``).
    """
    qmd = find_qmd_cli()
    if not qmd:
        return []
    try:
        proc = subprocess.run(
            [str(qmd), "search", query, "-c", QMD_COLLECTION,
             "--limit", str(limit)],
            capture_output=True, text=True, timeout=timeout, check=False,
        )
    except (subprocess.TimeoutExpired, OSError):
        return []
    if proc.returncode != 0 or not proc.stdout:
        return []
    return parse_qmd_output(proc.stdout, docs_root=docs_root)


def parse_qmd_output(text: str, docs_root: Path | None = None) -> list[Learning]:
    """Convert qmd's text output to Learning objects by reading each hit's file.

    qmd emits lines like `qmd://learnings/learnings/<file>.md:<line> #hash`
    for each result. We extract the relative path, resolve it under the QMD
    collection root, and parse frontmatter + body.
    """
    seen: set[str] = set()
    learnings: list[Learning] = []
    root = docs_root if docs_root is not None else QMD_DOCS_ROOT  # R15
    for m in QMD_PATH_RE.finditer(text):
        rel = m.group(1)
        if rel in seen:  # qmd can emit multiple line hits per file
            continue
        seen.add(rel)
        path = root / rel
        try:
            content = path.read_text()
        except OSError:
            continue
        fm, body = parse_frontmatter(content)
        archived = None
        am = ARCHIVE_HEADER_RE.search(body)
        if am:
            archived = am.group(1)
        learnings.append(Learning(chunk_text=content, frontmatter=fm, archived_at=archived))
    return learnings


def _coerce_datetime(raw: Any) -> datetime | None:
    """R5: coerce one frontmatter/header value to a NAIVE datetime.

    yaml.safe_load already turns ISO timestamps into datetime (tz-aware for
    a trailing Z) and bare dates into date objects; raw strings come from
    the ``<!-- archived: ... -->`` body header. tzinfo is dropped rather
    than converted — the R6 window is day-granular and naive, and a few
    hours of offset never flips a day-window verdict.
    """
    if isinstance(raw, datetime):
        return raw.replace(tzinfo=None)
    if isinstance(raw, date):
        return datetime(raw.year, raw.month, raw.day)
    if isinstance(raw, str):
        try:
            return datetime.fromisoformat(raw.strip().rstrip("Z")).replace(
                tzinfo=None
            )
        except ValueError:
            return None
    return None


def learning_timestamp(learning: Learning) -> datetime | None:
    """R5: coalesce a learning's effective timestamp for window matching.

    Frontmatter ``archived`` → ``updated_at`` → ``created`` → ``date``,
    then the ``<!-- archived: ... -->`` body header (what R8's recency
    boost reads). Mirrors Hindsight's COALESCE(occurred_start,
    mentioned_at, occurred_end) date coalescing — first known timestamp
    wins. None when the learning carries no parsable date: undatable notes
    are simply invisible to the temporal arm, never guessed into a window.
    """
    for key in ("archived", "updated_at", "created", "date"):
        dt = _coerce_datetime(learning.frontmatter.get(key))
        if dt is not None:
            return dt
    return _coerce_datetime(learning.archived_at)


def fetch_temporal(
    temporal: TemporalRange | None, limit: int, query: str = "",
    docs_root: Path | None = None,
) -> list[Learning]:
    """R5: temporal retrieval arm — date-window scan of the learnings corpus.

    Hindsight's ``retrieve_temporal_combined`` runs a similarity-ranked,
    window-filtered SQL arm next to semantic/BM25/graph and only when
    extraction found a constraint. Port shape: walk the local corpus
    (QMD_DOCS_ROOT, the same files qmd indexes), keep learnings whose
    coalesced timestamp falls inside ``[temporal.start, temporal.end]``,
    and rank them

        1. by lexical overlap with the date-stripped query (the stdlib
           analog of Hindsight's similarity-first pool selection), then
        2. by temporal proximity to the window midpoint (Hindsight's
           ``temporal_proximity = 1 − |d − mid| / (span/2)``).

    Contract: returns [] when ``temporal`` is None (date-free queries get
    ZERO hits from this arm — no false boost), when the arm is disabled,
    when the corpus is absent, or on any IO error. Booster, never blocker.
    """
    if temporal is None or not TEMPORAL_ARM_ENABLED or limit <= 0:
        return []
    # R15: scope the corpus scan to the chosen shard when the caller passed a
    # docs_root. Otherwise honour the engine's KB override (the eval harness
    # and any isolated caller set GLOBAL_LEARNINGS_PATH to a sandbox KB whose
    # documents live under <base>/documents) — scanning the user's live
    # corpus from inside a sandboxed run would leak real learnings.
    if docs_root is not None:
        root = docs_root
    else:
        override = os.environ.get("GLOBAL_LEARNINGS_PATH")
        root = Path(override) / "documents" if override else QMD_DOCS_ROOT
    try:
        if not root.is_dir():
            return []
        paths = sorted(root.rglob("*.md"))[:TEMPORAL_ARM_MAX_FILES]
    except OSError:
        return []

    # Overlap against the query MINUS the matched date phrase — "decisions
    # last week" should rank on "decisions", not on notes mentioning "week".
    topical_query = query.lower()
    if temporal.matched_text:
        topical_query = topical_query.replace(temporal.matched_text, " ")

    span = (temporal.end - temporal.start).total_seconds()
    mid = temporal.start + (temporal.end - temporal.start) / 2
    scored: list[tuple[float, float, Learning]] = []
    for path in paths:
        try:
            content = path.read_text()
        except (OSError, UnicodeDecodeError):
            continue  # unreadable/binary stray — booster, never blocker
        fm, body = parse_frontmatter(content)
        archived = None
        m = ARCHIVE_HEADER_RE.search(body)
        if m:
            archived = m.group(1)
        lrn = Learning(chunk_text=content, frontmatter=fm, archived_at=archived)
        ts = learning_timestamp(lrn)
        if ts is None or not (temporal.start <= ts <= temporal.end):
            continue
        if span > 0:
            proximity = 1.0 - min(
                abs((ts - mid).total_seconds()) / (span / 2.0), 1.0
            )
        else:
            proximity = 1.0
        overlap = (
            lexical_overlap(topical_query, lrn) if topical_query.strip() else 0.0
        )
        scored.append((overlap, proximity, lrn))
    # Stable sort: ties keep the deterministic path order.
    scored.sort(key=lambda t: (t[0], t[1]), reverse=True)
    return [lrn for _, _, lrn in scored[:limit]]


def _learning_key(learning: Learning) -> str:
    """Dedup key stable across backends. Prefers frontmatter id, falls back to
    a hash of the chunk so distinct chunks don't collapse."""
    fid = learning.frontmatter.get("id") or learning.frontmatter.get("name")
    if fid:
        return str(fid)
    return hashlib.sha1(learning.chunk_text[:256].encode()).hexdigest()[:12]


def rrf_fuse(result_lists: list[list[Learning]], k: int = RRF_K) -> list[Learning]:
    """Reciprocal Rank Fusion. Standard hybrid-search technique.

    score(doc) = Σ 1 / (k + rank_in_each_source)

    Source-agnostic — doesn't need score normalization across backends.
    Docs appearing in both get summed scores → fused ranking.
    """
    scores: dict[str, float] = {}
    first_seen: dict[str, Learning] = {}
    for results in result_lists:
        for rank, learning in enumerate(results, start=1):
            key = _learning_key(learning)
            scores[key] = scores.get(key, 0.0) + 1.0 / (k + rank)
            # Keep the first occurrence (prefer full-chunk from learnings search
            # over file-read from qmd when both are present)
            if key not in first_seen:
                first_seen[key] = learning
    ordered_keys = sorted(scores, key=lambda key: scores[key], reverse=True)
    return [first_seen[key] for key in ordered_keys]


def _ce_sigmoid(raw: float) -> float:
    """R2: map a cross-encoder logit to (0, 1).

    ms-marco models emit unbounded logits (≈ -12 … +12); sigmoid keeps the
    primary sort key positive and bounded so the legacy-formula modifier
    can never flip its sign or explode it.
    """
    try:
        return 1.0 / (1.0 + math.exp(-raw))
    except OverflowError:
        return 0.0 if raw < 0 else 1.0


def fetch_ce_scores(
    cli: Path,
    query: str,
    learnings: list[Learning],
    timeout: int = CE_TIMEOUT,
    env: dict[str, str] | None = None,
) -> dict[str, float] | None:
    """R2: score the top fused candidates via `reflect rerank` (cross-encoder).

    Sends one batch of up to CE_CANDIDATES (query, chunk) pairs to the
    engine, which holds the heavy sentence-transformers dependency and the
    model cache (~/.reflect/models/; auto-download on first use).

    Returns {learning_key: raw_logit} or None on ANY failure — slim build
    without sentence-transformers, legacy CLI without the subcommand,
    timeout while the model downloads, junk output. The cross-encoder is a
    booster, never a blocker.
    """
    if not learnings:
        return None
    payload = json.dumps({
        "candidates": [
            {"id": _learning_key(lrn), "text": lrn.chunk_text[:2000]}
            for lrn in learnings[:CE_CANDIDATES]
        ]
    })
    try:
        proc = subprocess.run(
            [str(cli), "rerank", query],
            input=payload,
            capture_output=True, text=True, timeout=timeout, check=False,
            env=env,  # R15: scope the rerank to the chosen shard's KB
        )
    except (subprocess.TimeoutExpired, OSError):
        return None
    if proc.returncode != 0 or not proc.stdout:
        return None
    try:
        data = json.loads(proc.stdout)
    except json.JSONDecodeError:
        return None
    if not isinstance(data, dict) or not data.get("available"):
        return None
    scores = data.get("scores")
    if not isinstance(scores, dict):
        return None
    return _coerce_ce_scores(scores)


def _coerce_ce_scores(raw: Any) -> dict[str, float] | None:
    """R2: validate a {key: logit} mapping from subprocess output or cache.

    Returns None on any shape problem so the caller degrades to the legacy
    formula instead of crashing on a hand-edited cache file.
    """
    if not isinstance(raw, dict):
        return None
    out: dict[str, float] = {}
    for key, value in raw.items():
        try:
            out[str(key)] = float(value)
        except (TypeError, ValueError):
            return None
    return out or None


def fetch_embeddings(
    cli: Path,
    query: str,
    learnings: list[Learning],
    timeout: int = EMBED_TIMEOUT,
    env: dict[str, str] | None = None,
) -> tuple[list[float], dict[str, list[float]]] | None:
    """R3: embed the query + top fused candidates via `reflect embed`.

    The engine embeds with the SAME all-mpnet-base-v2 model nano-graphrag
    indexes with, so MMR's similarity lives in the index's embedding space.

    Returns (query_vector, {learning_key: vector}) or None on ANY failure —
    slim build without sentence-transformers, legacy CLI without the
    subcommand, timeout while the model loads, junk output. MMR is a
    booster, never a blocker.
    """
    if not learnings:
        return None
    payload = json.dumps({
        "candidates": [
            {"id": _learning_key(lrn), "text": lrn.chunk_text[:2000]}
            for lrn in learnings[:MMR_CANDIDATES]
        ]
    })
    try:
        proc = subprocess.run(
            [str(cli), "embed", query],
            input=payload,
            capture_output=True, text=True, timeout=timeout, check=False,
            env=env,  # R15: scope the embed to the chosen shard's KB
        )
    except (subprocess.TimeoutExpired, OSError):
        return None
    if proc.returncode != 0 or not proc.stdout:
        return None
    try:
        data = json.loads(proc.stdout)
    except json.JSONDecodeError:
        return None
    if not isinstance(data, dict) or not data.get("available"):
        return None
    return _coerce_embeddings({
        "query": data.get("query_embedding"),
        "docs": data.get("embeddings"),
    })


def _coerce_vector(raw: Any) -> list[float] | None:
    """R3: validate one embedding vector — a non-empty list of numbers."""
    if not isinstance(raw, list) or not raw:
        return None
    out: list[float] = []
    for value in raw:
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            return None
        out.append(float(value))
    return out


def _coerce_embeddings(
    raw: Any,
) -> tuple[list[float], dict[str, list[float]]] | None:
    """R3: validate {"query": [...], "docs": {key: [...]}} from subprocess
    output or a cache file.

    Returns (query_vector, {key: vector}) or None on any shape problem —
    including dimension mismatches, which would silently corrupt cosines —
    so the caller degrades to plain top-k slicing instead of crashing.
    """
    if not isinstance(raw, dict):
        return None
    query_vec = _coerce_vector(raw.get("query"))
    docs_raw = raw.get("docs")
    if query_vec is None or not isinstance(docs_raw, dict) or not docs_raw:
        return None
    dim = len(query_vec)
    docs: dict[str, list[float]] = {}
    for key, value in docs_raw.items():
        vec = _coerce_vector(value)
        if vec is None or len(vec) != dim:
            return None
        docs[str(key)] = vec
    return query_vec, docs


def _cosine(a: list[float], b: list[float]) -> float:
    """R3: cosine similarity. Engine vectors are unit-normalized, but cached
    or hand-edited ones may not be — guard the norms instead of trusting them."""
    dot = 0.0
    norm_a = 0.0
    norm_b = 0.0
    for x, y in zip(a, b):
        dot += x * y
        norm_a += x * x
        norm_b += y * y
    if norm_a <= 0.0 or norm_b <= 0.0:
        return 0.0
    return dot / math.sqrt(norm_a * norm_b)


def mmr_select(
    learnings: list[Learning],
    embeddings: tuple[list[float], dict[str, list[float]]] | None,
    k: int,
    lam: float | None = None,
    rel_scores: dict[str, float] | None = None,
) -> list[Learning]:
    """R3: Maximal Marginal Relevance selection of the final top-k.

    Keeps the top reranked hit, then repeatedly picks
        argmax( λ·rel(d,q) − (1−λ)·max_{s∈selected} cos(d, s) )
    over the remaining embedded candidates.

    rel(d,q) is the candidate's RERANK score (``rel_scores`` from
    rerank_with_scores — the cross-encoder × formula blend, recency
    included) normalized by the window max so it lands in (0, 1] next to
    the cosine penalty. Deriving rel from query↔doc cosine instead would
    silently drop the CE and recency signal for slots 2+ (eval showed it
    resurrecting superseded conventions); the bi-encoder cosine is only a
    fallback when scores are absent.

    Candidates without an embedding (beyond the MMR_CANDIDATES window)
    keep their reranked order and only fill slots the embedded head can't.
    Without embeddings (slim engine, --no-mmr upstream, stale cache) this
    is exactly ``learnings[:k]``.

    Ties resolve to the earlier (higher-reranked) candidate — strict ``>``
    comparison keeps the selection deterministic.
    """
    if k <= 0:
        return []
    if lam is None:
        lam = MMR_LAMBDA
    lam = min(1.0, max(0.0, lam))
    if not embeddings or len(learnings) <= 1:
        return learnings[:k]
    query_vec, doc_vecs = embeddings
    if _learning_key(learnings[0]) not in doc_vecs:
        return learnings[:k]  # window/result mismatch — don't guess
    head = [lrn for lrn in learnings if _learning_key(lrn) in doc_vecs]
    tail = [lrn for lrn in learnings if _learning_key(lrn) not in doc_vecs]

    max_score = 0.0
    if rel_scores:
        max_score = max(
            (rel_scores.get(_learning_key(lrn), 0.0) for lrn in head),
            default=0.0,
        )

    def _rel(lrn: Learning) -> float:
        key = _learning_key(lrn)
        if rel_scores and max_score > 0 and key in rel_scores:
            return rel_scores[key] / max_score
        return _cosine(query_vec, doc_vecs[key])

    rel = {_learning_key(lrn): _rel(lrn) for lrn in head}
    selected = [head[0]]
    remaining = head[1:]
    # Incrementally maintained max similarity to the selected set: O(n·k)
    # cosines instead of recomputing the max each round.
    max_sim = {
        _learning_key(lrn): _cosine(
            doc_vecs[_learning_key(lrn)], doc_vecs[_learning_key(head[0])]
        )
        for lrn in remaining
    }
    while remaining and len(selected) < k:
        best_idx = 0
        best_val = -math.inf
        for idx, cand in enumerate(remaining):
            key = _learning_key(cand)
            val = lam * rel[key] - (1.0 - lam) * max_sim[key]
            if val > best_val:
                best_idx, best_val = idx, val
        picked = remaining.pop(best_idx)
        selected.append(picked)
        picked_vec = doc_vecs[_learning_key(picked)]
        for cand in remaining:
            key = _learning_key(cand)
            sim = _cosine(doc_vecs[key], picked_vec)
            if sim > max_sim[key]:
                max_sim[key] = sim
    if len(selected) < k:
        selected.extend(tail[: k - len(selected)])
    return selected


def parse_learnings_output(json_blob: str) -> list[Learning]:
    """Split a `reflect search --format json` response into Learning objects."""
    try:
        envelope = json.loads(json_blob)
    except json.JSONDecodeError:
        return []
    # Expected shape is {"context": "...chunks...--New Chunk--..."}.
    # Guard against list/string/other shapes so a CLI format change can't
    # crash us — it should just return zero results.
    if not isinstance(envelope, dict):
        return []
    context = envelope.get("context", "")
    if not isinstance(context, str) or not context:
        return []
    chunks = [c.strip() for c in context.split(CHUNK_SEPARATOR) if c.strip()]
    results: list[Learning] = []
    for chunk in chunks:
        fm, body = parse_frontmatter(chunk)
        archived = None
        m = ARCHIVE_HEADER_RE.search(body)
        if m:
            archived = m.group(1)
        results.append(Learning(chunk_text=chunk, frontmatter=fm, archived_at=archived))
    return results


def rerank(
    learnings: list[Learning],
    query_tags: list[str] | None = None,
    now: datetime | None = None,
    ce_scores: dict[str, float] | None = None,
    current_project: str | None = None,
) -> list[Learning]:
    """
    D8 + S4 + R8 + R16 + SG3: score = CE × confidence_boost × recency_boost
    × tag_boost × proof_count_boost × project_affinity_boost
    × speculative_boost — every boost multiplicative and bounded to ±α/2
    (Hindsight ``apply_combined_scoring`` shape; see :func:`bounded_boost`).

    R16: ``current_project`` scopes the affinity boost — None (default)
    auto-detects via :func:`detect_current_project`; "" disables matching
    (the future R15 shard-scoped path passes "" so affinity only applies
    when scope is global).

    R2: when ``ce_scores`` (cross-encoder logits keyed by learning key) are
    present, semantic relevance becomes the PRIMARY sort key and the
    bounded-boost formula is a multiplicative modifier:

        score = sigmoid(ce_logit) × boost_product

    Candidates without a CE score (beyond the CE_CANDIDATES batch) take the
    CE_UNSCORED epsilon — they sort below every scored candidate, ordered
    by the boost product among themselves. Without CE entirely (slim build,
    legacy CLI) the boost product is the whole score.

    Sorts in-place and returns the same list.
    """
    learnings, _ = rerank_with_scores(
        learnings, query_tags, now, ce_scores, current_project
    )
    return learnings


def rerank_with_scores(
    learnings: list[Learning],
    query_tags: list[str] | None = None,
    now: datetime | None = None,
    ce_scores: dict[str, float] | None = None,
    current_project: str | None = None,
) -> tuple[list[Learning], dict[str, float]]:
    """R3: :func:`rerank` + the final per-candidate scores it sorted by.

    Identical ordering contract to ``rerank``; additionally returns
    ``{learning_key: score}`` so mmr_select can reuse the rerank's OWN
    relevance (the cross-encoder × formula blend, including recency) as
    rel(d,q) instead of re-deriving it from bi-encoder cosines — which
    would silently drop the CE and recency signal for slots 2+.
    """
    now = now or datetime.now(tz=None)
    qt = set(t.lower() for t in (query_tags or []))
    if current_project is None:
        # R16: only pay for project detection (one git subprocess, memoized)
        # when the boost is live AND at least one candidate declares a
        # project — otherwise every norm is neutral anyway.
        current_project = (
            detect_current_project()
            if PROJECT_AFFINITY_ALPHA > 0.0
            and any(lrn.project_id for lrn in learnings)
            else ""
        )

    def formula(lrn: Learning) -> float:
        # R8 + R16: product of bounded multiplicative boosts — each signal
        # worth at most ±α/2, so no single one can dominate the ordering.
        # S3: confidence rides the continuous 0–1 value (confidence_num),
        # normed so tier-only notes score exactly as before.
        return (
            bounded_boost(confidence_num_norm(lrn.confidence_num), CONFIDENCE_ALPHA)
            * bounded_boost(recency_norm(lrn.archived_at, now), RECENCY_ALPHA)
            * bounded_boost(tag_norm(qt, lrn.tags), TAG_ALPHA)
            * proof_count_boost(lrn.proof_count)
            * bounded_boost(
                project_norm(current_project, lrn.project_id),
                PROJECT_AFFINITY_ALPHA,
            )
            * bounded_boost(speculative_norm(lrn.tags), SPECULATIVE_ALPHA)
        )

    def score(lrn: Learning) -> float:
        if ce_scores is None:
            return formula(lrn)
        raw = ce_scores.get(_learning_key(lrn))
        ce = _ce_sigmoid(raw) if raw is not None else CE_UNSCORED
        return ce * formula(lrn)

    scores: dict[str, float] = {}
    for lrn in learnings:
        scores.setdefault(_learning_key(lrn), score(lrn))
    learnings.sort(key=lambda lrn: scores[_learning_key(lrn)], reverse=True)
    return learnings, scores


def bounded_boost(norm: float, alpha: float) -> float:
    """R8: the Hindsight bounded-boost shape: ``1 + α·(norm − 0.5)``.

    ``norm`` is clamped to [0, 1] first, so the multiplier is guaranteed to
    stay within [1 − α/2, 1 + α/2] whatever the upstream normalizer emits.
    norm = 0.5 is exactly neutral (multiplier 1.0).
    """
    norm = min(1.0, max(0.0, norm))
    return 1.0 + alpha * (norm - 0.5)


def _normalize_project(raw: Any) -> str:
    """R16: normalize a project identifier for the affinity match.

    Accepts a bare name ("my-app") or a path ("/Users/x/dev/my-app" — what a
    CLAUDE_PROJECT_DIR-derived writer records); either way the comparison key
    is the lowercase final path component. Empty/None → "" (no project).
    """
    if raw is None or isinstance(raw, bool):
        return ""
    text = str(raw).strip().strip("/")
    if not text:
        return ""
    return text.rsplit("/", 1)[-1].strip().lower()


_CURRENT_PROJECT_CACHE: str | None = None


def detect_current_project() -> str:
    """R16: the current session's project id, memoized per process.

    Resolution order (mirrors output_generator.get_project_dir):
      1. $CLAUDE_PROJECT_DIR — set by Claude Code hooks/skills.
      2. `git rev-parse --show-toplevel` — repo root of the cwd.
    Returns "" when neither resolves (non-git scratch dir, git missing) —
    the affinity boost is then neutral for every hit.
    """
    global _CURRENT_PROJECT_CACHE
    if _CURRENT_PROJECT_CACHE is not None:
        return _CURRENT_PROJECT_CACHE
    project = _normalize_project(os.environ.get("CLAUDE_PROJECT_DIR"))
    if not project:
        try:
            proc = subprocess.run(
                ["git", "rev-parse", "--show-toplevel"],
                capture_output=True, text=True, timeout=5, check=False,
            )
        except (subprocess.TimeoutExpired, OSError):
            proc = None
        if proc is not None and proc.returncode == 0:
            project = _normalize_project(proc.stdout.strip())
    _CURRENT_PROJECT_CACHE = project
    return project


_CURRENT_BRANCH_CACHE: str | None = None


def detect_current_branch() -> str:
    """A6: the current git branch/worktree branch, sanitized, memoized.

    Resolution: ``git rev-parse --abbrev-ref HEAD`` run in the session cwd —
    worktree-aware, so each worktree (``/.agents-in-a-box/worktrees/...``)
    reports the branch ITS HEAD points at, which is exactly the isolation
    boundary we shard on. $RECALL_BRANCH overrides the detection (the
    SessionStart hook already knows the branch; tests pin it).

    The result is passed through :func:`_sanitize_branch`, so trunk branches
    (main/master), a detached HEAD, or no git context all return "" — and the
    caller then uses the project-level shard (R15's path). Memoized per
    process like :func:`detect_current_project`.
    """
    global _CURRENT_BRANCH_CACHE
    if _CURRENT_BRANCH_CACHE is not None:
        return _CURRENT_BRANCH_CACHE
    override = os.environ.get("RECALL_BRANCH")
    if override is not None:
        branch = _sanitize_branch(override)
    else:
        try:
            proc = subprocess.run(
                ["git", "rev-parse", "--abbrev-ref", "HEAD"],
                capture_output=True, text=True, timeout=5, check=False,
            )
        except (subprocess.TimeoutExpired, OSError):
            proc = None
        branch = (
            _sanitize_branch(proc.stdout.strip())
            if proc is not None and proc.returncode == 0
            else ""
        )
    _CURRENT_BRANCH_CACHE = branch
    return branch


def project_norm(current_project: str, hit_project: str) -> float:
    """R16: same-project match → 1.0 (boost ceiling 1 + α/2); everything
    else — cross-project, unknown current project, project-less learning —
    sits at the neutral 0.5 so the multiplier is EXACTLY 1.0. There is no
    below-neutral side: cross-project hits are down-RANKED relative to
    same-project ties, never down-SCORED below their R8 baseline."""
    if current_project and hit_project and current_project == hit_project:
        return 1.0
    return 0.5


def confidence_norm(tier: str) -> float:
    """R8: confidence tier → [0, 1]. HIGH=1.0, MEDIUM=0.5 (neutral),
    LOW=0.0; unknown tiers sit at the neutral baseline.

    S3 note: the rerank formula now goes through
    :func:`confidence_num_norm`; this tier version is kept for bucket-edge
    consumers (``--confidence`` filtering, display) and is anchored to it —
    ``confidence_num_norm(tier midpoint) == confidence_norm(tier)``.
    """
    return CONFIDENCE_NORMS.get(tier, 0.5)


def confidence_num_norm(num: float) -> float:
    """S3: numeric confidence (0–1) → [0, 1] boost norm.

    Linear rescale anchored on the tier midpoints so tier-only legacy notes
    keep their EXACT pre-S3 norms — 0.9→1.0, 0.6→0.5 (neutral), 0.3→0.0 —
    while notes carrying a calibrated ``confidence_num`` land proportionally
    between bucket edges (the finer-grained ranking S3 buys). Clamped, so
    <=0.3 floors at 0.0 and >=0.9 ceilings at 1.0.
    """
    return min(1.0, max(0.0, (num - 0.3) / 0.6))


def recency_norm(archived_at: str | None, now: datetime) -> float:
    """R8: linear recency decay over RECENCY_WINDOW_DAYS → [0.1, 1.0];
    neutral 0.5 when the date is missing or unparsable (Hindsight shape).

    The 0.1 floor means even ancient notes keep a toehold; the old
    exp(-age/90) multiplier crushed them to ~0 instead.
    """
    if not archived_at:
        return 0.5
    try:
        ts = datetime.fromisoformat(archived_at.rstrip("Z"))
        days_ago = (now - ts).total_seconds() / 86400.0
    except (ValueError, TypeError):
        # TypeError: aware-vs-naive datetime subtraction (one side has a
        # +00:00 offset). ValueError: malformed ISO string. Either way,
        # fall back to neutral rather than crashing the rerank over one
        # bad archive header.
        return 0.5
    return max(0.1, min(1.0, 1.0 - days_ago / RECENCY_WINDOW_DAYS))


def tag_norm(query_tags: set[str], learning_tags: list[str]) -> float:
    """R8: query-tag coverage → [0, 1]. No query tags → neutral 0.5 (boost
    collapses to 1.0, matching how Hindsight neutralizes absent signals).
    With query tags, the norm is the overlap fraction — full coverage 1.0,
    none 0.0."""
    if not query_tags:
        return 0.5
    lt = set(t.lower() for t in learning_tags)
    return len(query_tags & lt) / len(query_tags)


def speculative_norm(learning_tags: list[str]) -> float:
    """SG3: speculative-tagged learning → 0.0 (boost floor 1 − α/2);
    everything else sits at the neutral 0.5 so the multiplier is EXACTLY
    1.0. Idle-triggered reflections carry the tag because the session may
    resume and overturn them — they should lose ties against learnings
    harvested from explicit session ends, not vanish."""
    if any(str(t).strip().lower() == SPECULATIVE_TAG for t in learning_tags):
        return 0.0
    return 0.5


def proof_count_boost(proof_count: int | None) -> float:
    """S4 + R8: log-normalized evidence multiplier, bounded to ±5% (α=0.1).

    proof_norm = clamp(0.5 + ln(proof_count)/10, 0, 1); missing or
    single-proof learnings sit exactly at the neutral 0.5 baseline so the
    multiplier is precisely 1.0 — legacy notes rank identically to before.
    """
    if proof_count is not None and proof_count >= 1:
        proof_norm = 0.5 + math.log(proof_count) / 10.0
    else:
        proof_norm = 0.5
    return bounded_boost(proof_norm, PROOF_COUNT_ALPHA)


def filter_by_confidence(learnings: list[Learning], threshold: str) -> list[Learning]:
    """threshold ∈ {HIGH, MEDIUM, LOW, ANY}"""
    if threshold == "ANY":
        return learnings
    rank = {"HIGH": 3, "MEDIUM": 2, "LOW": 1}
    min_rank = rank.get(threshold, 0)
    return [lrn for lrn in learnings if rank.get(lrn.confidence, 0) >= min_rank]


def _content_terms(text: str) -> set[str]:
    return {
        t for t in re.findall(r"[a-z0-9][a-z0-9_\-]{2,}", text.lower())
        if t not in _STOPWORDS
    }


def lexical_overlap(query: str, learning: Learning) -> float:
    """R7: fraction of the query's content terms present in the chunk.

    The engine always returns its nearest neighbours — even for a query about
    something the KB has never seen. Confidence/recency scores can't tell
    "relevant" from "nearest junk"; query-term coverage can (cheaply, stdlib).
    Hyphen/underscore-split variants count so `kill-server` matches `kill`+
    `server` phrasing and vice versa.
    """
    q_terms = _content_terms(query)
    if not q_terms:
        return 1.0  # vacuous query — never gate
    text = learning.chunk_text.lower()
    hits = 0
    for term in q_terms:
        if term in text:
            hits += 1
            continue
        parts = [p for p in re.split(r"[-_]", term) if len(p) >= 3]
        if parts and all(p in text for p in parts):
            hits += 1
    return hits / len(q_terms)


def apply_ood_gate(
    learnings: list[Learning], query: str, min_overlap: float
) -> tuple[list[Learning], bool]:
    """R7: suppress the result set when even the BEST hit barely mentions the
    query's terms. Returns (learnings, gated)."""
    if min_overlap <= 0 or not learnings:
        return learnings, False
    best = max(lexical_overlap(query, lrn) for lrn in learnings[:5])
    if best < min_overlap:
        return [], True
    return learnings, False


def apply_arm_floor(
    learnings: list[Learning], query: str, arm: str
) -> list[Learning]:
    """R12: drop an ARM's candidates whose query-term coverage is below that
    arm's calibrated min_score floor, BEFORE the candidates enter RRF fusion.

    Layered on R7 (which gates the FUSED set post-fusion with a single global
    threshold): the global gate can't distinguish a vector-arm near-miss from a
    BM25-arm near-miss because RRF erases the source. R12 gates each arm at the
    source with its own floor, so a borderline hit one arm should drop (its
    native score barely clears that arm's bar) is removed before it can win a
    fusion rank — while the SAME hit survives on an arm whose floor is lower.

    Per-candidate (not best-only like R7): every candidate the arm emits is
    individually tested, so the gate tightens which candidates from this arm
    reach fusion rather than all-or-nothing suppressing the arm. A floor of 0
    is a no-op (returns the list unchanged — pre-R12 behaviour)."""
    floor = _arm_floor(arm)
    if floor <= 0 or not learnings:
        return learnings
    return [lrn for lrn in learnings if lexical_overlap(query, lrn) >= floor]


def _percentile(values: list[float], pct: float) -> float:
    """The *pct* (0–100) percentile of *values* (nearest-rank). [] => 0.0."""
    if not values:
        return 0.0
    ordered = sorted(values)
    idx = int(round((pct / 100.0) * (len(ordered) - 1)))
    return ordered[max(0, min(len(ordered) - 1, idx))]


def calibrate_thresholds(
    *,
    scope_global: bool = False,
    all_branches: bool = False,
    sample_size: int = 40,
    percentile: float = 10.0,
) -> str:
    """R12: sample the corpus to DERIVE per-arm min_score floors.

    For each of a random sample of corpus documents we build an in-domain probe
    query from that document's own title + key_insight (these terms ARE in the
    note, so a healthy arm should surface it), run the relevant arm, and record
    the query-term coverage of the candidates that arm returns. The arm's floor
    is then set just below the in-domain distribution — the ``percentile``-th
    percentile of observed in-domain coverage — so genuine in-domain hits clear
    the floor while out-of-domain near-misses (which score below the in-domain
    body) are dropped. Because the arms produce different coverage distributions
    (the dense vector arm surfaces lexically-distant neighbours the lexical BM25
    arm never would), each arm gets its OWN derived floor — which is the whole
    point of R12.

    The entity-graph and temporal arms are not lexically calibrated: the graph
    arm expands by entity neighbourhood (a related note can share zero query
    terms), so a lexical floor would wrongly drop its legitimate hits — it ships
    at 0 (open). The temporal arm is already date-window scoped, so it keeps the
    light default floor. Output is paste-ready TOML + env exports.
    """
    cli = find_learnings_cli()
    subproc_env, kb_root = recall_env(scope_global, all_branches)
    docs_root = _docs_root_for(kb_root)
    floors = dict(CALIBRATED_FLOORS)  # graph/temporal keep their shipped values
    samples: dict[str, list[float]] = {"vector": [], "bm25": []}

    docs = sorted(docs_root.rglob("*.md")) if docs_root.is_dir() else []
    if cli and docs:
        random.shuffle(docs)
        for path in docs[:sample_size]:
            try:
                content = path.read_text()
            except (OSError, UnicodeDecodeError):
                continue
            fm, _body = parse_frontmatter(content)
            lrn = Learning(chunk_text=content, frontmatter=fm)
            probe = " ".join(filter(None, [lrn.title, lrn.key_insight])).strip()
            if not probe or probe == "(no title)":
                continue
            # Vector arm = the primary `reflect search` mode.
            try:
                proc = subprocess.run(
                    [str(cli), "search", probe, "--mode", DEFAULT_MODE,
                     "--format", "json", "--limit", "10"],
                    capture_output=True, text=True, timeout=60, check=False,
                    env=subproc_env,
                )
                if proc.returncode == 0:
                    for cand in parse_learnings_output(proc.stdout):
                        samples["vector"].append(lexical_overlap(probe, cand))
            except (subprocess.TimeoutExpired, OSError):
                pass
            # BM25 arm = qmd.
            for cand in fetch_qmd(probe, 10, docs_root=docs_root):
                samples["bm25"].append(lexical_overlap(probe, cand))

    for arm in ("vector", "bm25"):
        if samples[arm]:
            # Floor sits just under the in-domain distribution: the low
            # percentile of observed in-domain coverage. Clamp to [0, 1].
            floors[arm] = round(min(1.0, max(0.0, _percentile(samples[arm], percentile))), 3)

    lines = [
        "# R12: per-arm calibrated OOD floors derived by `reflect "
        "calibrate-thresholds`.",
        f"# sampled docs={min(sample_size, len(docs))} percentile={percentile} "
        f"(vector n={len(samples['vector'])}, bm25 n={len(samples['bm25'])})",
        "[recall.arm.vector]",
        f"min_score = {floors['vector']}",
        "[recall.arm.bm25]",
        f"min_score = {floors['bm25']}",
        "[recall.arm.graph]",
        f"min_score = {floors['graph']}  # entity-neighbourhood arm — open",
        "[recall.arm.temporal]",
        f"min_score = {floors['temporal']}  # date-window scoped — light floor",
        "",
        "# env-var form (recall.py reads these directly):",
        f"# export RECALL_ARM_VECTOR_MIN_SCORE={floors['vector']}",
        f"# export RECALL_ARM_BM25_MIN_SCORE={floors['bm25']}",
        f"# export RECALL_ARM_GRAPH_MIN_SCORE={floors['graph']}",
        f"# export RECALL_ARM_TEMPORAL_MIN_SCORE={floors['temporal']}",
    ]
    return "\n".join(lines)


def filter_by_token_budget(
    learnings: list[Learning], max_tokens: int
) -> list[Learning]:
    """R4: return learnings until the token budget is spent (≥1 always kept,
    so a single long learning can't starve the caller)."""
    if max_tokens <= 0:
        return learnings
    out: list[Learning] = []
    spent = 0
    for lrn in learnings:
        cost = _est_tokens(lrn.chunk_text)
        if out and spent + cost > max_tokens:
            break
        out.append(lrn)
        spent += cost
    return out


# --- M8: token economics ---------------------------------------------------

def _plugin_scripts_dir() -> Path:
    """plugins/reflect/scripts — where mode_loader.py (M4) lives relative to
    this script (skills/recall/scripts/recall.py)."""
    return Path(__file__).resolve().parents[3] / "scripts"


def mode_glyphs() -> dict[str, str]:
    """M8: ``{learning_type_id: glyph}`` from the ACTIVE mode (M4).

    The mapping is declarative mode data, never a constant in this script:
    each mode's ``learning_types`` entries carry a ``work_emoji`` (claude-mem
    plugin/modes/code.json shape; the display ``emoji`` is the fallback per
    type). Active-mode resolution (env > project config > TOML > default)
    is mode_loader's job — re-resolved on every call so REFLECT_MODE /
    REFLECT_MODES_DIR changes take effect without a re-import.

    Silent-fail: any problem (mode_loader missing in a stripped deploy,
    broken mode file, empty modes dir) returns {} so economics degrade to
    the neutral fallback glyph instead of breaking recall.
    """
    try:
        scripts = _plugin_scripts_dir()
        if str(scripts) not in sys.path:
            sys.path.insert(0, str(scripts))
        import mode_loader

        mode = mode_loader.get_active_mode()
        glyphs: dict[str, str] = {}
        for entry in mode_loader.get_learning_types(mode):
            if not isinstance(entry, dict):
                continue
            tid = str(entry.get("id", "") or "").strip().lower()
            glyph = str(
                entry.get("work_emoji") or entry.get("emoji") or ""
            ).strip()
            if tid and glyph:
                glyphs[tid] = glyph
        return glyphs
    except Exception:
        return {}


def learning_economics(
    learning: Learning, glyphs: dict[str, str] | None = None
) -> dict[str, Any]:
    """M8: one learning's token economics (claude-mem
    formatObservationTokenDisplay shape).

    ``savings_pct`` = round((discovery − read) / discovery × 100); 0 when
    discovery is unknown-zero. Negative when the stored note is LONGER than
    its estimated discovery cost — surfaced honestly, not clamped.
    """
    read = learning.read_tokens
    discovery = learning.discovery_tokens
    pct = round((discovery - read) / discovery * 100) if discovery > 0 else 0
    if glyphs is None:
        glyphs = mode_glyphs()
    return {
        "discovery_tokens": discovery,
        "read_tokens": read,
        "savings_pct": pct,
        "glyph": glyphs.get(learning.learning_type)
        or ECONOMICS_FALLBACK_GLYPH,
    }


def block_economics(learnings: list[Learning]) -> dict[str, int]:
    """M8: per-block roll-up (claude-mem calculateTokenEconomics shape):
    total injected (read) tokens, total discovery tokens, saved = the
    difference, and the block-level savings percentage."""
    read = sum(lrn.read_tokens for lrn in learnings)
    discovery = sum(lrn.discovery_tokens for lrn in learnings)
    saved = discovery - read
    pct = round(saved / discovery * 100) if discovery > 0 else 0
    return {
        "count": len(learnings),
        "read_tokens": read,
        "discovery_tokens": discovery,
        "saved_tokens": saved,
        "savings_pct": pct,
    }


def _economics_row(econ: dict[str, Any]) -> str:
    """M8: the per-row display — ``<glyph> D:<n> → R:<n> (-<pct>%)``.
    A net-negative row renders ``(+<pct>%)`` so a bloated note is visible."""
    pct = econ["savings_pct"]
    sign = "-" if pct >= 0 else "+"
    return (
        f"{econ['glyph']} D:{econ['discovery_tokens']} → "
        f"R:{econ['read_tokens']} ({sign}{abs(pct)}%)"
    )


def render_markdown(
    learnings: list[Learning],
    query: str,
    max_chars: int = DEFAULT_MAX_CHARS,
    field: str | None = None,
) -> str:
    """D5: compact markdown block for agent context.

    S1: ``field`` projects each hit to ONE structured frontmatter field
    ("just the rule" instead of a paragraph). Legacy notes without the field
    fall back to the matching body section, then key_insight/title — never
    dropped, never crash.

    M8: every row carries its token economics next to the mode's type glyph
    (``<glyph> D:<n> → R:<n> (-<pct>%)``) and the header line rolls up the
    block totals — recall is self-justifying about its ROI. Disable with
    RECALL_ECONOMICS=0 for byte-identical pre-M8 output.
    """
    if not learnings:
        return ""
    title = f"## Prior learnings relevant to `{query[:80]}`"
    if field:
        title += f" (field: {field})"
    glyphs = mode_glyphs() if ECONOMICS_ENABLED else {}
    if ECONOMICS_ENABLED:
        totals = block_economics(learnings)
        title += (
            f" — {totals['count']} learnings, "
            f"~{totals['read_tokens']} tok injected, "
            f"est ~{totals['saved_tokens']} tok saved"
        )
    lines = [title + "\n"]
    used = len(lines[0])
    for lrn in learnings:
        suffix = ""
        if ECONOMICS_ENABLED:
            suffix = " — " + _economics_row(learning_economics(lrn, glyphs))
        if field:
            value = lrn.field_value(field) or lrn.key_insight or lrn.title
            entry = f"- **[{lrn.id}]** {value}{suffix}\n"
        else:
            header = f"- **[{lrn.id}]** {lrn.key_insight or lrn.title}{suffix}"
            how = lrn.how_to_apply
            entry = header + (f"\n  How to apply: {how}" if how else "") + "\n"
        if used + len(entry) > max_chars:
            lines.append(f"- _(…{len(learnings) - (len(lines) - 1)} more truncated)_\n")
            break
        lines.append(entry)
        used += len(entry)
    return "".join(lines).rstrip() + "\n"


def render_json(
    learnings: list[Learning],
    query: str,
    mode: str,
    ood_gated: bool = False,
    temporal: TemporalRange | None = None,
    field: str | None = None,
) -> str:
    """S1: when ``field`` is set, each result additionally carries
    ``field_value`` — the projected frontmatter field (body-section fallback
    for legacy notes), or null when the note genuinely has nothing. JSON is
    programmatic, so unlike the markdown rendering it never substitutes
    key_insight/title — callers can tell "has the field" from "doesn't".

    M8: each result carries an ``economics`` object (discovery_tokens /
    read_tokens / savings_pct / glyph) and the envelope a block-total
    ``economics`` roll-up; both null/absent when RECALL_ECONOMICS=0."""
    glyphs = mode_glyphs() if ECONOMICS_ENABLED else {}

    def _result(lrn: Learning) -> dict[str, Any]:
        row: dict[str, Any] = {
            "id": lrn.id,
            "title": lrn.title,
            "key_insight": lrn.key_insight,
            "confidence": lrn.confidence,
            # S3: the continuous value ranking used (tier above is the
            # display bucket — Hindsight maps to buckets at API edges only).
            "confidence_num": lrn.confidence_num,
            "tags": lrn.tags,
            "how_to_apply": lrn.how_to_apply,
            "archived_at": lrn.archived_at,
        }
        if field:
            row["field_value"] = lrn.field_value(field) or None
        if ECONOMICS_ENABLED:
            # M8: per-result token economics next to the mode glyph.
            row["economics"] = learning_economics(lrn, glyphs)
        return row

    return json.dumps(
        {
            "query": query,
            "mode": mode,
            "count": len(learnings),
            "ood_gated": ood_gated,
            # R6: the date range parsed out of the query, or null.
            "temporal": temporal.to_dict() if temporal else None,
            # S1: the projected field name, or null when no projection.
            "field": field or None,
            # M8: block-total token economics, or null when disabled.
            "economics": (
                block_economics(learnings) if ECONOMICS_ENABLED else None
            ),
            "results": [_result(lrn) for lrn in learnings],
        },
        indent=2,
    )


def log_recall(
    query: str, mode: str, count: int, cached: bool,
    cache_tier: str | None = None,
    economics: dict[str, int] | None = None,
) -> None:
    """D_phase6: append-only jsonl for future helpfulness tracking.

    R9: ``cache_tier`` records WHICH tier answered ("exact" / "fuzzy" /
    None) so the fuzzy hit-rate over a session is measurable from the log.

    M8: ``economics`` (the block_economics roll-up) lands as
    injected_tokens / discovery_tokens / saved_tokens / savings_pct so the
    A4 followup-rate diagnostic can correlate economics with recall quality.
    """
    base = Path(os.environ.get("REFLECT_STATE_DIR", Path.home() / ".reflect"))
    log = base / "recall_log.jsonl"
    record = {
        "ts": datetime.now().isoformat(timespec="seconds"),
        "query": query,
        "mode": mode,
        "count": count,
        "cached": cached,
        "cache_tier": cache_tier,
    }
    if economics is not None:
        record["injected_tokens"] = economics.get("read_tokens", 0)
        record["discovery_tokens"] = economics.get("discovery_tokens", 0)
        record["saved_tokens"] = economics.get("saved_tokens", 0)
        record["savings_pct"] = economics.get("savings_pct", 0)
    try:
        base.mkdir(parents=True, exist_ok=True)
        with log.open("a") as f:
            f.write(json.dumps(record) + "\n")
    except OSError:
        pass


def normalize_gap_query(query: str) -> str:
    """SG6: stable dedup key for a knowledge-gap entry.

    Lowercased, stopword-filtered content terms (the SAME tokenizer the R7
    OOD gate uses), sorted so word-order variants of one ask collapse to a
    single key — "tmux kill server" and "kill tmux server" are the same
    gap. Returns "" for vacuous queries (all stopwords / short tokens);
    those are never logged.
    """
    return " ".join(sorted(_content_terms(query)))


def _gap_session_id(explicit: str | None) -> str:
    """SG6: session identity for cross-session repeat detection.

    Prefers the caller-supplied id (``--session-id`` from a hook), then
    $CLAUDE_SESSION_ID. Without either, falls back to a per-day pseudo-id
    so anonymous repeats on DIFFERENT days still count as distinct
    sessions while N asks within one anonymous day count as one.
    """
    sid = (explicit or os.environ.get("CLAUDE_SESSION_ID", "") or "").strip()
    if sid:
        return sid
    return "unknown-" + datetime.now().strftime("%Y-%m-%d")


def log_knowledge_gap(query: str, session_id: str | None = None) -> None:
    """SG6: append a 0-result recall to ``~/.reflect/knowledge-gaps.jsonl``.

    Negative recall is unused information — silently dropping an empty
    result hides exactly the queries the KB SHOULD cover. Append-only
    jsonl with the same silent-fail contract as :func:`log_recall`: a
    logging failure must never break the recall path.
    """
    if not GAP_LOG_ENABLED:
        return
    normalized = normalize_gap_query(query)
    if not normalized:
        return  # vacuous query — not a meaningful gap
    base = Path(os.environ.get("REFLECT_STATE_DIR", Path.home() / ".reflect"))
    log = base / "knowledge-gaps.jsonl"
    try:
        base.mkdir(parents=True, exist_ok=True)
        with log.open("a") as f:
            f.write(
                json.dumps(
                    {
                        "ts": datetime.now().isoformat(timespec="seconds"),
                        "query": query[:200],
                        "normalized": normalized,
                        "session_id": _gap_session_id(session_id),
                    }
                )
                + "\n"
            )
    except OSError:
        pass


def _recent_searches_path() -> Path:
    """A4: per-session most-recent-search state file."""
    base = Path(os.environ.get("REFLECT_STATE_DIR", Path.home() / ".reflect"))
    return base / "recent-searches.json"


def _metrics_jsonl_path() -> Path:
    """A4: the engine's append-only metrics log (~/.learnings/metrics.jsonl,
    the file reflect-kb's write_metric appends to). REFLECT_METRICS_PATH
    overrides for tests / relocated installs."""
    override = (os.environ.get("REFLECT_METRICS_PATH") or "").strip()
    if override:
        return Path(override).expanduser()
    return Path.home() / ".learnings" / "metrics.jsonl"


def track_followup(
    query: str,
    result_ids: list[str],
    session_id: str,
    *,
    now: float | None = None,
) -> bool:
    """A4: record this search as the session's most recent and report whether
    it is a followup to the previous one.

    A followup = the session's prior search happened within the window,
    asked something DIFFERENT (same query inside the window is a retry from
    a flaky caller, not a followup), and its result-id set shares NOTHING
    with this one — the agent searched again because the first answer didn't
    satisfy. Stale per-session entries are pruned on every write (the
    on-read analog of agentmemory's hourly sweep). State-file IO is
    best-effort: an unreadable/corrupt file means "no prior", never a crash.
    """
    now = time.time() if now is None else now
    path = _recent_searches_path()
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
        if not isinstance(data, dict):
            data = {}
    except (OSError, json.JSONDecodeError, UnicodeDecodeError):
        data = {}

    prior = data.get(session_id)
    max_age = max(_followup_window_seconds(), FOLLOWUP_STATE_MAX_AGE_SECONDS)
    data = {
        sid: entry
        for sid, entry in data.items()
        if isinstance(entry, dict)
        and isinstance(entry.get("at"), (int, float))
        and 0 <= now - entry["at"] <= max_age
    }
    data[session_id] = {
        "query": query,
        "result_ids": [str(rid) for rid in result_ids],
        "at": now,
    }
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(data), encoding="utf-8")
    except OSError:
        pass

    if not isinstance(prior, dict):
        return False
    prior_at = prior.get("at")
    if not isinstance(prior_at, (int, float)):
        return False
    if not 0 <= now - prior_at <= _followup_window_seconds():
        return False
    if prior.get("query") == query:
        return False  # retry, not a followup
    raw_prior_ids = prior.get("result_ids")
    prior_ids = (
        {str(rid) for rid in raw_prior_ids}
        if isinstance(raw_prior_ids, list)
        else set()
    )
    if not prior_ids:
        return False
    return prior_ids.isdisjoint(str(rid) for rid in result_ids)


def log_followup_metric(
    query: str, session_id: str, followup: bool, result_count: int,
) -> None:
    """A4: append the search's followup verdict to metrics.jsonl.

    Same record shape as the engine's write_metric ({ts, op, …}); the
    followup rate over a window = mean(followup) across op="recall_search"
    lines. Silent-fail like every other log writer in this script.
    """
    path = _metrics_jsonl_path()
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("a", encoding="utf-8") as f:
            f.write(
                json.dumps(
                    {
                        "ts": datetime.now(timezone.utc).isoformat(),
                        "op": "recall_search",
                        "session_id": session_id,
                        "followup": bool(followup),
                        "window_seconds": _followup_window_seconds(),
                        "result_count": result_count,
                        "query": query[:200],
                    }
                )
                + "\n"
            )
    except OSError:
        pass


def record_followup_diagnostic(
    query: str,
    learnings: list[Learning],
    session_id: str | None,
    enabled: bool,
) -> None:
    """A4: shared tail for both recall() return paths. Never raises.

    Skips: diagnostic disabled (env gate ANDed with the caller's flag),
    empty result set (retrieval failure — SG6's territory, counting it
    would inflate the rate on every miss), or no session anchor (explicit
    --session-id, then $CLAUDE_SESSION_ID; SG6's per-day pseudo-id fallback
    is NOT used here — it would conflate parallel anonymous sessions inside
    the 30s window).
    """
    if not (FOLLOWUP_ENABLED and enabled) or not learnings:
        return
    sid = (session_id or os.environ.get("CLAUDE_SESSION_ID", "") or "").strip()
    if not sid:
        return
    try:
        ids = [_learning_key(lrn) for lrn in learnings]
        followup = track_followup(query, ids, sid)
        log_followup_metric(query, sid, followup, len(ids))
    except Exception:
        pass  # diagnostics must never break the recall path


# --- Core entry ----------------------------------------------------------

def recall(
    query: str,
    *,
    limit: int = DEFAULT_LIMIT,
    mode: str = DEFAULT_MODE,
    confidence: str = "ANY",
    max_chars: int = DEFAULT_MAX_CHARS,
    use_cache: bool = True,
    cache_ttl: int = DEFAULT_CACHE_TTL,
    query_tags: list[str] | None = None,
    max_tokens: int = 0,
    min_overlap: float = 0.0,
    use_mmr: bool = True,
    mmr_lambda: float | None = None,
    session_id: str | None = None,
    gap_log: bool = True,
    followup_track: bool = True,
    scope_global: bool = False,
    all_branches: bool = False,
) -> RecallResult:
    """High-level API: query → ranked Learnings. Never raises on KB issues.

    R15 + A6: ``scope_global`` False (default) scopes retrieval to the CURRENT
    project's shard, and — A6 — within it to the CURRENT branch's sub-shard
    (``~/.learnings/shards/<project>/branches/<branch>/``) so worktrees of one
    repo don't pollute each other; ``all_branches`` widens to the
    project-level shard (every branch). ``scope_global`` True searches the
    pooled global KB across all projects. An explicit pre-set
    $GLOBAL_LEARNINGS_PATH override always wins over all of these (the
    harness/eval contract). See :func:`resolve_kb_root`.

    R4: ``max_tokens`` > 0 bounds the result set by estimated tokens instead
    of count alone. R7: ``min_overlap`` > 0 suppresses out-of-domain results
    (top-hit query-term coverage below the threshold => empty set).
    R3: ``use_mmr`` (ANDed with the RECALL_MMR env gate) selects the final
    top-k with Maximal Marginal Relevance; ``mmr_lambda`` overrides the
    default λ (None => MMR_LAMBDA).
    SG6: a final empty result set (including OOD-gated empties — "nearest
    junk only" IS a gap) is appended to knowledge-gaps.jsonl keyed by
    ``session_id``; ``gap_log=False`` (ANDed with the RECALL_GAP_LOG env
    gate) opts a synthetic-query caller out. Infra errors (CLI missing,
    search failed) are NOT gaps and never logged.
    A4: non-empty results with a session anchor feed the followup-rate
    diagnostic (a second, different search within 30s returning a disjoint
    result set = recall didn't satisfy); ``followup_track=False`` (ANDed
    with the RECALL_FOLLOWUP env gate) opts a synthetic-query caller out.
    """
    mmr_on = MMR_ENABLED and use_mmr  # R3
    # R6: parse a natural-language date phrase out of the query up front so
    # every return path (errors included) carries the range. Extraction is a
    # stdlib regex pass that never raises — None when no phrase resolves.
    temporal = extract_temporal_constraint(query) if TEMPORAL_ENABLED else None
    cli = find_learnings_cli()
    if not cli:
        return RecallResult(
            [], query, mode,
            error="reflect CLI not found on $PATH (install with `uv tool install reflect-kb`)",
            temporal=temporal,
        )

    # R15: resolve which KB the `reflect` subprocess + corpus arms read. By
    # default this is the current project's shard; --global / RECALL_GLOBAL
    # selects the pooled KB; an explicit pre-set $GLOBAL_LEARNINGS_PATH
    # override leaves the env untouched (kb_root is None) and every arm reads
    # that override, exactly as before R15.
    subproc_env, kb_root = recall_env(scope_global, all_branches)  # A6
    docs_root = _docs_root_for(kb_root)
    # R16 × R15: when the corpus is already a single-project shard, the
    # project-affinity boost is redundant (every hit is same-project) — pass
    # current_project="" to neutralize it. Only the pooled-global path
    # (kb_root == GLOBAL_LEARNINGS_ROOT) lets affinity auto-detect and apply.
    # An explicit override (kb_root None) keeps the pre-R15 auto-detect.
    rerank_project: str | None = (
        "" if (kb_root is not None and kb_root != _global_learnings_root())
        else None
    )

    fetched_limit = max(limit * 2, 10)
    # R15: the cache is keyed by the SHARD too — the same query against two
    # projects' shards (or the pooled global KB) returns different result
    # sets, so they must not share a cache entry. Fold the resolved KB root
    # into the cache's mode component; the fuzzy index uses the same token.
    cache_mode = _cache_scope_token(mode, kb_root)
    cache_file = cache_path(query, cache_mode, fetched_limit)
    if use_cache:
        # R9: Tier 0 — exact-hash hit; Tier 1 — fuzzy fallback over the
        # token-set index (Jaccard ≥ FUZZY_CACHE_THRESHOLD, TTL still
        # enforced by read_cache inside fuzzy_read_cache).
        cached = read_cache(cache_file, cache_ttl)
        cache_tier = "exact" if cached else None
        if cached is None:
            cached = fuzzy_read_cache(
                query, cache_mode, fetched_limit, cache_ttl  # R15: shard-keyed
            )
            if cached is not None:
                cache_tier = "fuzzy"
        if cached:
            learnings = [
                Learning(
                    chunk_text=r.get("chunk_text", ""),
                    frontmatter=r.get("frontmatter", {}),
                    archived_at=r.get("archived_at"),
                )
                for r in cached.get("results", [])
            ]
            ce_scores = _coerce_ce_scores(cached.get("ce_scores")) if CROSS_ENCODER_ENABLED else None  # R2
            embeddings = _coerce_embeddings(cached.get("embeddings")) if mmr_on else None  # R3
            learnings, rank_scores = rerank_with_scores(
                learnings, query_tags, ce_scores=ce_scores,
                current_project=rerank_project,  # R15×R16
            )
            learnings = filter_by_confidence(learnings, confidence.upper())
            learnings, gated = apply_ood_gate(learnings, query, min_overlap)  # R7
            if mmr_on:  # R3
                learnings = mmr_select(
                    learnings, embeddings, k=limit, lam=mmr_lambda,
                    rel_scores=rank_scores,
                )
            else:
                learnings = learnings[:limit]
            learnings = filter_by_token_budget(learnings, max_tokens)  # R4
            log_recall(
                query, mode, len(learnings), cached=True,
                cache_tier=cache_tier,  # R9
                economics=(  # M8
                    block_economics(learnings) if ECONOMICS_ENABLED else None
                ),
            )
            if gap_log and not learnings:  # SG6
                log_knowledge_gap(query, session_id)
            record_followup_diagnostic(  # A4
                query, learnings, session_id, followup_track,
            )
            return RecallResult(
                learnings, query, mode, cache_hit=True,
                cache_tier=cache_tier, ood_gated=gated,
                scores=rank_scores, temporal=temporal,
            )

    # Fan out vector search (reflect CLI), QMD (BM25), R1's graph arm
    # (reflect `--mode local`, entity-neighborhood expansion), and — R5 —
    # the temporal arm (date-window corpus scan, only when R6 extraction
    # found a date phrase) in parallel. Every arm beyond the primary is a
    # booster, not a blocker — each returns [] on any failure and fusion
    # still works.
    def _fetch_mode(search_mode: str) -> tuple[list[Learning], str | None]:
        try:
            proc = subprocess.run(
                [str(cli), "search", query, "--mode", search_mode,
                 "--format", "json", "--limit", str(fetched_limit)],
                capture_output=True, text=True, timeout=60, check=False,
                env=subproc_env,  # R15: scope to the chosen shard's KB
            )
        except (subprocess.TimeoutExpired, OSError) as e:
            return [], f"subprocess failed: {e}"
        if proc.returncode != 0:
            return [], f"reflect search exit {proc.returncode}"
        return parse_learnings_output(proc.stdout), None

    # R1: only add the entity-graph arm when it differs from the primary mode
    # (an explicit `--mode local` call shouldn't fan out twice).
    graph_arm = GRAPH_ARM_ENABLED and mode != "local"
    # R5: the temporal arm only runs when the query carried a date phrase —
    # a date-free query contributes NOTHING from this arm (no false boost).
    temporal_arm = TEMPORAL_ARM_ENABLED and temporal is not None
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        learnings_future = pool.submit(_fetch_mode, mode)
        qmd_future = pool.submit(
            fetch_qmd, query, fetched_limit, docs_root=docs_root  # R15
        )
        graph_future = pool.submit(_fetch_mode, "local") if graph_arm else None
        temporal_future = (
            pool.submit(
                fetch_temporal, temporal, fetched_limit, query,
                docs_root=docs_root,  # R15
            )
            if temporal_arm else None
        )
        graph_results, graph_err = learnings_future.result()
        qmd_results = qmd_future.result()
        entity_results: list[Learning] = []
        if graph_future is not None:
            entity_results, _entity_err = graph_future.result()  # booster — errors ignored
        temporal_results: list[Learning] = []
        if temporal_future is not None:
            temporal_results = temporal_future.result()

    # If the primary path failed but a booster returned results, keep going.
    if graph_err and not qmd_results and not entity_results and not temporal_results:
        return RecallResult([], query, mode, error=graph_err, temporal=temporal)

    # R12: gate EACH arm by its own calibrated floor before fusion. The arms'
    # native scores are not comparable, so a single global gate (R7) can't be
    # right for all of them at once; each arm drops its own sub-floor candidates
    # here, then R7 still gates the fused set globally below. `graph_results` is
    # the PRIMARY mode's output — the dense vector arm in the default `naive`
    # mode — so it carries the "vector" floor; `entity_results` is the `--mode
    # local` entity-graph expansion arm.
    learnings = rrf_fuse(
        [
            apply_arm_floor(graph_results, query, "vector"),
            apply_arm_floor(qmd_results, query, "bm25"),
            apply_arm_floor(entity_results, query, "graph"),
            apply_arm_floor(temporal_results, query, "temporal"),
        ]
    )
    # R2: cross-encoder scores for the fused top candidates. R3: mpnet
    # embeddings for the same window (MMR diversity). Both shell out to the
    # engine, so they run concurrently — added MMR latency is max(), not
    # sum(). The cache is per-query, so the (query-dependent) CE scores and
    # embeddings are cached alongside the raw results — cache hits skip the
    # models entirely.
    ce_scores: dict[str, float] | None = None
    embeddings: tuple[list[float], dict[str, list[float]]] | None = None
    if CROSS_ENCODER_ENABLED or mmr_on:
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            ce_future = (
                pool.submit(
                    fetch_ce_scores, cli, query, learnings, env=subproc_env  # R15
                )
                if CROSS_ENCODER_ENABLED else None
            )
            emb_future = (
                pool.submit(
                    fetch_embeddings, cli, query, learnings, env=subproc_env  # R15
                )
                if mmr_on else None
            )
            if ce_future is not None:
                ce_scores = ce_future.result()
            if emb_future is not None:
                embeddings = emb_future.result()
    # persist raw results to cache before filtering (so different confidence/limit
    # combinations can reuse the same fetch)
    if use_cache:
        write_cache(
            cache_file,
            {
                "query": query,
                "mode": mode,
                "fetched_at": time.time(),
                "ce_scores": ce_scores,
                "embeddings": (
                    {"query": embeddings[0], "docs": embeddings[1]}
                    if embeddings else None
                ),
                "results": [
                    {
                        "chunk_text": l.chunk_text,
                        "frontmatter": l.frontmatter,
                        "archived_at": l.archived_at,
                    }
                    for l in learnings
                ],
            },
        )
        # R9: register this fetch's token set so near-identical future
        # queries can fuzzy-hit the payload just written.
        update_cache_index(query, cache_mode, fetched_limit, cache_file)  # R15
    learnings, rank_scores = rerank_with_scores(
        learnings, query_tags, ce_scores=ce_scores,
        current_project=rerank_project,  # R15×R16
    )
    learnings = filter_by_confidence(learnings, confidence.upper())
    learnings, gated = apply_ood_gate(learnings, query, min_overlap)  # R7
    if mmr_on:  # R3
        learnings = mmr_select(
            learnings, embeddings, k=limit, lam=mmr_lambda,
            rel_scores=rank_scores,
        )
    else:
        learnings = learnings[:limit]
    learnings = filter_by_token_budget(learnings, max_tokens)  # R4
    log_recall(
        query, mode, len(learnings), cached=False,
        economics=(  # M8
            block_economics(learnings) if ECONOMICS_ENABLED else None
        ),
    )
    if gap_log and not learnings:  # SG6
        log_knowledge_gap(query, session_id)
    record_followup_diagnostic(query, learnings, session_id, followup_track)  # A4
    return RecallResult(
        learnings, query, mode, ood_gated=gated, scores=rank_scores,
        temporal=temporal,
    )


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("query", nargs="*", help="Search query")
    ap.add_argument(
        "--calibrate-thresholds", action="store_true",
        help="R12: instead of querying, SAMPLE the corpus to derive per-arm "
             "min_score floors and print them as TOML ([recall.arm.<name>]) "
             "plus the matching RECALL_ARM_<NAME>_MIN_SCORE env exports. "
             "Each arm's floor is set at a conservative low percentile of the "
             "in-domain query-term coverage distribution that arm produces — "
             "so genuine in-domain hits clear it while out-of-domain near-"
             "misses are dropped. The arms are not score-comparable, so each "
             "gets its own floor.")
    ap.add_argument("--limit", type=int, default=DEFAULT_LIMIT)
    ap.add_argument("--mode", choices=["naive", "local", "global"], default=DEFAULT_MODE)
    ap.add_argument("--confidence", choices=["HIGH", "MEDIUM", "LOW", "ANY"], default="ANY")
    ap.add_argument("--format", choices=["markdown", "json"], default="markdown")
    ap.add_argument("--max-chars", type=int, default=DEFAULT_MAX_CHARS)
    ap.add_argument("--no-cache", action="store_true")
    ap.add_argument("--cache-ttl", type=int, default=DEFAULT_CACHE_TTL)
    ap.add_argument("--tags", default="",
                    help="Comma-separated query tags for tag-overlap reranking")
    ap.add_argument("--max-tokens", type=int, default=0,
                    help="R4: bound results by estimated tokens (0 = no budget)")
    ap.add_argument("--min-overlap", type=float, default=0.0,
                    help="R7: OOD gate — suppress results when the best hit's "
                         "query-term coverage is below this (0 = off)")
    ap.add_argument("--no-mmr", action="store_true",
                    help="R3: disable MMR diversity selection (benchmarking)")
    ap.add_argument("--mmr-lambda", type=float, default=None,
                    help="R3: MMR relevance↔diversity trade-off λ in [0,1] "
                         f"(default {MMR_LAMBDA}; 1.0 = pure relevance)")
    ap.add_argument("--session-id", default=None,
                    help="SG6: session id recorded with knowledge-gap entries "
                         "(falls back to $CLAUDE_SESSION_ID, then a per-day "
                         "pseudo-id)")
    ap.add_argument("--no-gap-log", action="store_true",
                    help="SG6: don't record a 0-result run as a knowledge gap "
                         "(synthetic-query callers like the SessionStart hook)")
    ap.add_argument("--no-followup", action="store_true",
                    help="A4: don't feed this search into the followup-rate "
                         "diagnostic (synthetic-query callers like the "
                         "SessionStart hook)")
    ap.add_argument("--field", default=None, metavar="NAME",
                    help="S1: project each hit to ONE structured frontmatter "
                         "field (e.g. rule, fix, root_cause, problem) instead "
                         "of the full note. Legacy notes fall back to the "
                         "matching body section, then key_insight/title.")
    ap.add_argument("--global", dest="scope_global", action="store_true",
                    help="R15: search the POOLED global KB across all "
                         "projects instead of the current project's shard "
                         "(~/.learnings/shards/<project>/). Default scope is "
                         "the current project's shard.")
    ap.add_argument("--all-branches", dest="all_branches", action="store_true",
                    help="A6: widen scope from the CURRENT branch's sub-shard "
                         "(~/.learnings/shards/<project>/branches/<branch>/) "
                         "to the project-level shard, pooling every branch of "
                         "the current project. Default scope is the current "
                         "branch only (worktree isolation).")
    args = ap.parse_args()

    if args.calibrate_thresholds:  # R12
        print(calibrate_thresholds(
            scope_global=args.scope_global, all_branches=args.all_branches,
        ))
        return 0

    query = " ".join(args.query).strip()
    if not query:
        print("error: empty query", file=sys.stderr)
        return 2

    query_tags = [t.strip() for t in args.tags.split(",") if t.strip()]

    result = recall(
        query,
        limit=args.limit,
        mode=args.mode,
        confidence=args.confidence,
        max_chars=args.max_chars,
        use_cache=not args.no_cache,
        cache_ttl=args.cache_ttl,
        query_tags=query_tags,
        max_tokens=args.max_tokens,
        min_overlap=args.min_overlap,
        use_mmr=not args.no_mmr,
        mmr_lambda=args.mmr_lambda,
        session_id=args.session_id,
        gap_log=not args.no_gap_log,
        followup_track=not args.no_followup,
        scope_global=args.scope_global,  # R15
        all_branches=args.all_branches,  # A6
    )

    if result.error:
        # D9: silent no-op on KB absence; only print to stderr when diagnostic
        if os.environ.get("REFLECT_RECALL_DEBUG"):
            print(f"recall: {result.error}", file=sys.stderr)
        # Empty output, exit 0
        return 0

    field = (args.field or "").strip() or None  # S1
    if args.format == "json":
        print(render_json(
            result.learnings, query, args.mode, result.ood_gated,
            temporal=result.temporal, field=field,
        ))
    else:
        out = render_markdown(
            result.learnings, query, max_chars=args.max_chars, field=field,
        )
        if out:
            print(out)

    return 0


if __name__ == "__main__":
    sys.exit(main())
