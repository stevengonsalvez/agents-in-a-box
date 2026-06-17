# ABOUTME: Behavioral proof for M7 — build_corpus snapshots EXACTLY the learnings matching a saved filter.
# ABOUTME: Pure file/frontmatter logic — no LLM, no embedding model; seeds + filter fully determine the selection.
"""M7 knowledge-corpus Q&A proof (build -> prime -> query -> reprime).

True invariant (decisive, no LLM):

  ``build_corpus(name, filter)`` scans the KB ``documents/`` dir and snapshots
  EXACTLY the learnings whose frontmatter matches the filter
  (tag / category / project / date-window) into
  ``$REFLECT_STATE_DIR/corpora/<name>.json``. A learning that matches is IN;
  one that fails any predicate is OUT. The seeds on disk plus the filter fully
  determine the selection — it is a deterministic, testable selection.

  The snapshot PERSISTS: a fresh process (we re-import / re-read the JSON from
  disk via ``load_corpus``) recovers the same entries plus the saved filter and
  a last-built timestamp.

  ``rebuild_corpus`` re-runs the SAVED filter against the current KB: a newly
  added matching learning is pulled IN, and a now-removed (deleted) learning is
  dropped OUT — re-priming on drift, driven only by the persisted filter.

  A KB write (mtime change) marks the corpus STALE (``is_stale`` True) — the
  reprime trigger.

Why no LLM: the corpus build/filter/snapshot/reprime path is pure frontmatter +
file IO. The conversational Q&A (asking questions over the primed slice) is the
calling agent's job per plugins/reflect/skills/corpus/SKILL.md and is NOT
exercised here. The unit under test is the deterministic selection: literal
frontmatter tags/category/project/created dates + a fixed filter wholly
determine which learnings land in the snapshot. No embedding model loads, so
this proof runs in milliseconds.

Isolation: every arm uses pytest's per-test ``tmp_path`` for both the KB
``documents/`` dir (``GLOBAL_LEARNINGS_PATH``) and the corpora state dir
(``REFLECT_STATE_DIR``), so arms share no mutable state and order is irrelevant.

PORT: M7
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

# Reuse the conftest's doc renderer so seeds carry the EXACT frontmatter shape
# the production engine + recall.py expect. conftest.py sits one dir up.
_CONFTEST_DIR = Path(__file__).resolve().parents[1]
if str(_CONFTEST_DIR) not in sys.path:
    sys.path.insert(0, str(_CONFTEST_DIR))
from conftest import _doc_md  # noqa: E402

from reflect_kb.recall import corpus as corpus_mod  # noqa: E402


# --- seed helpers ---------------------------------------------------------

def _seed_kb(tmp_path: Path, monkeypatch, learnings: list[dict]) -> Path:
    """Write learning .md files into a hermetic KB and point env at it.

    Returns the documents/ dir. No reflect reindex needed — the corpus filter
    reads the .md frontmatter directly.
    """
    kb = tmp_path / "kb"
    docs = kb / "documents"
    docs.mkdir(parents=True, exist_ok=True)
    state = tmp_path / "state"
    state.mkdir(parents=True, exist_ok=True)
    for d in learnings:
        (docs / f"{d['name']}.md").write_text(_doc_md(d))
    monkeypatch.setenv("GLOBAL_LEARNINGS_PATH", str(kb))
    monkeypatch.setenv("REFLECT_STATE_DIR", str(state))
    return docs


# A fixed seed set spanning every filterable dimension.
SEEDS = [
    dict(name="auth-jwt", title="JWT refresh race", category="security",
         tags=["auth", "jwt"], project_id="api", created="2026-02-01",
         key_insight="Lock the refresh", body="Auth token refresh body."),
    dict(name="auth-session", title="Session fixation", category="security",
         tags=["auth", "session"], project_id="api", created="2026-05-10",
         key_insight="Rotate on login", body="Session body."),
    dict(name="db-migrate", title="Online migration", category="database",
         tags=["migration", "ddl"], project_id="api", created="2026-03-01",
         key_insight="Backfill async", body="Migration body."),
    dict(name="ui-toast", title="Toast a11y", category="frontend",
         tags=["ui", "a11y"], project_id="web", created="2026-04-01",
         key_insight="aria-live polite", body="Toast body."),
]


# --- Arm A: filter selects EXACTLY the matching learnings -----------------

def test_M7_filter_selects_exactly_matching_learnings(tmp_path, monkeypatch):
    _seed_kb(tmp_path, monkeypatch, SEEDS)

    # tag:auth — only the two security/auth notes; db + ui are OUT.
    filt = corpus_mod.CorpusFilter(tags=("auth",))
    corpus = corpus_mod.build_corpus("auth-subsystem", filt)

    assert corpus.ids == ["auth-jwt", "auth-session"], corpus.ids
    assert "db-migrate" not in corpus.ids
    assert "ui-toast" not in corpus.ids

    # category:database — exactly the one db note.
    cat = corpus_mod.build_corpus("db", corpus_mod.CorpusFilter(category="database"))
    assert cat.ids == ["db-migrate"]

    # project:web — exactly the one web note (auth notes are project api).
    web = corpus_mod.build_corpus("web", corpus_mod.CorpusFilter(project="web"))
    assert web.ids == ["ui-toast"]

    # date window since/until — only notes whose created date is in-window.
    # 2026-02-15..2026-04-15 includes db-migrate (03-01) and ui-toast (04-01),
    # excludes auth-jwt (02-01, before) and auth-session (05-10, after).
    win = corpus_mod.build_corpus(
        "q1", corpus_mod.CorpusFilter(since="2026-02-15", until="2026-04-15"))
    assert win.ids == ["db-migrate", "ui-toast"], win.ids

    # AND semantics: tag:auth AND project:api AND since 2026-03 -> only the
    # may auth-session note (jwt is 02-01, before the window).
    combo = corpus_mod.build_corpus(
        "auth-recent",
        corpus_mod.CorpusFilter(tags=("auth",), project="api", since="2026-03-01"))
    assert combo.ids == ["auth-session"], combo.ids


# --- Arm B: snapshot persists across a fresh re-read ----------------------

def test_M7_snapshot_persists_to_disk(tmp_path, monkeypatch):
    _seed_kb(tmp_path, monkeypatch, SEEDS)

    filt = corpus_mod.CorpusFilter(tags=("auth",))
    built = corpus_mod.build_corpus("auth-subsystem", filt)

    # The file exists where the spec mandates: corpora/<name>.json under state.
    path = corpus_mod.corpus_path("auth-subsystem")
    assert path.exists()
    assert path.parent.name == "corpora"
    assert path.name == "auth-subsystem.json"

    # Re-read from disk (simulating a fresh process) recovers entries, the
    # SAVED filter, and the last-built timestamp.
    reloaded = corpus_mod.load_corpus("auth-subsystem")
    assert reloaded is not None
    assert reloaded.ids == built.ids == ["auth-jwt", "auth-session"]
    assert reloaded.filt.tags == ("auth",)
    assert reloaded.built_at and reloaded.built_at == built.built_at
    assert reloaded.kb_mtime > 0

    # The primed Q&A document carries each admitted learning's content.
    doc = reloaded.prime_document()
    assert "JWT refresh race" in doc
    assert "Session fixation" in doc
    assert "Online migration" not in doc  # not in this corpus


# --- Arm C: rebuild re-runs the saved filter (add in / drop out) ----------

def test_M7_rebuild_adds_new_and_drops_removed(tmp_path, monkeypatch):
    docs = _seed_kb(tmp_path, monkeypatch, SEEDS)

    corpus_mod.build_corpus("auth-subsystem", corpus_mod.CorpusFilter(tags=("auth",)))

    # Add a NEW matching learning (tag:auth) and DELETE an existing match.
    (docs / "auth-mfa.md").write_text(_doc_md(dict(
        name="auth-mfa", title="MFA bypass", category="security",
        tags=["auth", "mfa"], project_id="api", created="2026-06-01",
        key_insight="Enforce step-up", body="MFA body.")))
    (docs / "auth-jwt.md").unlink()  # now-removed match

    # rebuild uses ONLY the persisted filter (no filter arg) — re-prime on drift.
    rebuilt = corpus_mod.rebuild_corpus("auth-subsystem")
    assert "auth-mfa" in rebuilt.ids          # new match pulled IN
    assert "auth-jwt" not in rebuilt.ids       # removed match dropped OUT
    assert "auth-session" in rebuilt.ids       # untouched match stays
    assert rebuilt.ids == ["auth-mfa", "auth-session"], rebuilt.ids

    # Persisted on disk too.
    assert corpus_mod.load_corpus("auth-subsystem").ids == ["auth-mfa", "auth-session"]


# --- Arm D: KB write marks the corpus stale (reprime trigger) -------------

def test_M7_kb_write_marks_corpus_stale(tmp_path, monkeypatch):
    docs = _seed_kb(tmp_path, monkeypatch, SEEDS)

    corpus = corpus_mod.build_corpus("auth-subsystem", corpus_mod.CorpusFilter(tags=("auth",)))
    # Freshly built against the current KB -> NOT stale.
    assert corpus_mod.is_stale(corpus) is False

    # A KB write bumps the documents mtime above the snapshot's recorded mtime.
    # Force a strictly-later mtime so the proof is robust on coarse fs clocks.
    new = docs / "auth-mfa.md"
    new.write_text(_doc_md(dict(
        name="auth-mfa", title="MFA bypass", category="security",
        tags=["auth"], project_id="api", created="2026-06-01",
        key_insight="step-up", body="MFA body.")))
    import os
    future = corpus.kb_mtime + 100
    os.utime(new, (future, future))

    # The reloaded corpus (from before the write) is now STALE — reprime needed.
    reloaded = corpus_mod.load_corpus("auth-subsystem")
    assert corpus_mod.is_stale(reloaded) is True

    # And rebuilding clears staleness (new mtime captured).
    rebuilt = corpus_mod.rebuild_corpus("auth-subsystem")
    assert corpus_mod.is_stale(rebuilt) is False


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-q", "-p", "no:cacheprovider"]))
