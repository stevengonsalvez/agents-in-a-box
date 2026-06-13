# ABOUTME: Pytest fixture `behavioral_kb` — a hermetic real-engine KB a proof can
# ABOUTME: seed with learnings, reindex, and query via recall.py the way SessionStart does.
"""Behavioral-proof harness.

A *behavioral proof* SEEDs specific learnings into a hermetic, real-engine KB,
ACTs by running recall.py the way SessionStart does, and ASSERTs an OBSERVABLE
invariant on the returned ranking / inclusion / exclusion / metadata. No LLM
participates in the assertion — the seeds plus flags fully determine the
outcome, so the proof is deterministic.

Hermetic isolation (same pattern proven in tests/eval/harness.py):
  - GLOBAL_LEARNINGS_PATH -> tmp KB built from the proof's seeds
  - REFLECT_STATE_DIR     -> tmp state (recall cache, logs)
  - XDG_CACHE_HOME        -> tmp (qmd arm sees an empty index; engine-only recall)
  - HF_HOME / SENTENCE_TRANSFORMERS_HOME -> the REAL home caches, so the ~420MB
    embedding model is reused instead of re-downloaded into the tmp dir.

Full-stack `reflect`: the global install may be the slim build (no embeddings).
Set RECALL_EVAL_BIN_DIR=/tmp/recall-eval-venv/bin so both this harness and
recall.py's `reflect` subprocess resolve the full-stack CLI.

Public API (see BehavioralKB):
    kb.seed([{name,title,tags,confidence,created,archived?,key_insight,body,
              entities?,rels?}, ...])        # writes docs + sidecars, reindexes
    kb.recall(query, **flags) -> dict        # {results:[{id,...}], ood_gated, count}
    kb.recall_ids(query, **flags) -> [id]    # convenience: just the ranked ids

Flags map to recall.py CLI / env:
    limit, confidence, min_overlap, max_tokens, mode, tags, no_mmr, mmr_lambda
    env={"RECALL_GRAPH_ARM": "0", ...}       # arbitrary env overrides for this call
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import time
from pathlib import Path

import pytest

HERE = Path(__file__).parent
EVAL_ROOT = HERE.parent  # reflect-kb/tests/eval

# Resolve recall.py the same way tests/eval/harness.py does. EVAL_ROOT is
# reflect-kb/tests/eval, so parents[2] is the repo root where plugins/ lives
# alongside reflect-kb/; parents[1].parent covers a standalone reflect-kb
# checkout with the plugin as a sibling dir.
_CANDIDATES = [
    EVAL_ROOT.parents[2] / "plugins" / "reflect" / "skills" / "recall" / "scripts" / "recall.py",
    EVAL_ROOT.parents[1].parent / "plugins" / "reflect" / "skills" / "recall" / "scripts" / "recall.py",
]
RECALL_PY = next((p for p in _CANDIDATES if p.exists()), _CANDIDATES[0])
if not RECALL_PY.exists():
    raise RuntimeError(f"recall.py not found; tried: {[str(p) for p in _CANDIDATES]}")


class HarnessError(RuntimeError):
    pass


def _run(cmd: list[str], env: dict, timeout: int = 1800) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, capture_output=True, text=True, env=env, timeout=timeout)


def _doc_md(d: dict) -> str:
    """Frontmatter + body in the exact shape the engine + recall.py expect.

    Mirrors fixtures/make_corpus.py:doc_md so seeds index identically to the
    committed corpus. An optional `archived` HTML comment carries the temporal
    signal the recency arm reads.
    """
    lines = [
        "---",
        f"name: {d['name']}",
        f'title: "{d["title"]}"',
        f"category: {d.get('category', 'general')}",
        "tags:",
        *[f"  - {t}" for t in d.get("tags", [])],
        f"confidence: {d.get('confidence', 'medium')}",
        f'created: "{d.get("created", "2026-01-01")}"',
        f'key_insight: "{d.get("key_insight", "")}"',
        "---",
        "",
    ]
    fm = "\n".join(lines)
    archived = f"<!-- archived: {d['archived']} -->\n\n" if d.get("archived") else ""
    body = d.get("body", "")
    insight = d.get("key_insight", "")
    return fm + archived + f"## Learning\n\n{body}\n\n**How to apply:** {insight}\n"


def _sidecar_yaml(d: dict) -> str | None:
    """Entity sidecar (drives the graph arm). None when the seed has no entities."""
    entities = d.get("entities") or []
    if not entities:
        return None
    ents = "\n".join(
        f'  - name: "{n}"\n    type: {t}\n    description: "{desc}"'
        for n, t, desc in entities
    )
    rels_list = d.get("rels") or []
    rels = "\n".join(
        f'  - source: "{s}"\n    target: "{t}"\n    type: {ty}\n'
        f'    description: "{desc}"\n    strength: {st}'
        for s, t, ty, desc, st in rels_list
    )
    created = d.get("created", "2026-01-01")
    out = (
        f"document_id: {d['name']}\n"
        f"extracted_at: '{created}T00:00:00'\n"
        f"entities:\n{ents}\n"
    )
    if rels:
        out += f"relationships:\n{rels}\n"
    else:
        out += "relationships: []\n"
    return out


class BehavioralKB:
    """A seedable, queryable hermetic real-engine KB for one proof."""

    def __init__(self, workdir: Path):
        self.workdir = Path(workdir)
        self.kb_dir = self.workdir / "kb"
        self.state_dir = self.workdir / "state"
        self.cache_home = self.workdir / "xdg-cache"
        self._initialized = False

    # ---------- environment ----------
    def env(self, extra: dict | None = None) -> dict:
        env = dict(os.environ)
        env["GLOBAL_LEARNINGS_PATH"] = str(self.kb_dir)
        env["REFLECT_STATE_DIR"] = str(self.state_dir)
        env["XDG_CACHE_HOME"] = str(self.cache_home)
        env.setdefault("HF_HOME", str(Path.home() / ".cache" / "huggingface"))
        env.setdefault(
            "SENTENCE_TRANSFORMERS_HOME",
            str(Path.home() / ".cache" / "torch" / "sentence_transformers"),
        )
        bin_dir = os.environ.get("RECALL_EVAL_BIN_DIR")
        if bin_dir:
            env["PATH"] = f"{bin_dir}:{env.get('PATH', '')}"
        if extra:
            env.update({k: str(v) for k, v in extra.items()})
        return env

    # ---------- init / seed ----------
    def _init(self) -> None:
        if self._initialized:
            return
        if not shutil.which("reflect", path=self.env()["PATH"]):
            raise HarnessError(
                "`reflect` CLI not resolvable — set RECALL_EVAL_BIN_DIR to a "
                "full-stack venv bin dir (e.g. /tmp/recall-eval-venv/bin)."
            )
        for d in (self.kb_dir, self.state_dir, self.cache_home):
            d.mkdir(parents=True, exist_ok=True)
        r = _run(["reflect", "init"], self.env())
        if r.returncode != 0:
            raise HarnessError(f"reflect init failed: {r.stderr[-600:]}")
        (self.kb_dir / "documents").mkdir(exist_ok=True)
        self._initialized = True

    def seed(self, learnings: list[dict]) -> None:
        """Write each learning's .md (+ optional .entities.yaml) and reindex.

        Each dict: {name, title, tags, confidence, created, archived?,
        key_insight, body, category?, entities?, rels?}. `name` must be unique
        and is the id recall.py returns. Calling seed() again is additive — it
        appends documents and reindexes the union.
        """
        self._init()
        docs = self.kb_dir / "documents"
        for d in learnings:
            (docs / f"{d['name']}.md").write_text(_doc_md(d))
            side = _sidecar_yaml(d)
            if side is not None:
                (docs / f"{d['name']}.entities.yaml").write_text(side)
        r = _run(["reflect", "reindex", "--force"], self.env(), timeout=1800)
        if r.returncode != 0:
            raise HarnessError(f"reflect reindex failed: {r.stderr[-800:]}")

    # ---------- query ----------
    def recall(self, query: str, *, limit: int = 5, confidence: str = "ANY",
               min_overlap: float = 0.0, max_tokens: int = 0, mode: str | None = None,
               tags: str = "", no_mmr: bool = False, mmr_lambda: float | None = None,
               env: dict | None = None, extra_args: list[str] | None = None) -> dict:
        """Run recall.py exactly as SessionStart does; return the parsed JSON dict.

        Returns {"count": int, "ood_gated": bool, "results": [{"id", ...}], ...}.
        --no-cache is always passed so each call re-runs the real pipeline.

        mode=None (the default) lets recall.py pick its own DEFAULT_MODE — which
        is `naive`, the mode SessionStart actually runs. naive returns raw
        per-doc chunks whose `name:` frontmatter survives as the result `id`;
        the `global`/`local` GraphRAG modes synthesize community-report context
        and DROP per-doc ids (results come back as `?`), so a proof asserting on
        a specific id must stay on the default. Pass mode explicitly only when a
        proof is deliberately exercising the graph arm.
        """
        cmd = [
            "python3", str(RECALL_PY), query,
            "--limit", str(limit),
            "--format", "json",
            "--no-cache",
            "--confidence", confidence,
            "--min-overlap", str(min_overlap),
            "--max-tokens", str(max_tokens),
        ]
        if mode is not None:
            cmd += ["--mode", mode]
        if tags:
            cmd += ["--tags", tags]
        if no_mmr:
            cmd += ["--no-mmr"]
        if mmr_lambda is not None:
            cmd += ["--mmr-lambda", str(mmr_lambda)]
        if extra_args:
            cmd += extra_args
        r = _run(cmd, self.env(env), timeout=300)
        if r.returncode != 0:
            raise HarnessError(
                f"recall.py exited {r.returncode}\nSTDERR:\n{r.stderr[-1200:]}"
            )
        try:
            return json.loads(r.stdout or "{}")
        except json.JSONDecodeError as exc:
            raise HarnessError(
                f"recall.py returned non-JSON:\n{r.stdout[:1200]}"
            ) from exc

    def recall_ids(self, query: str, **flags) -> list[str]:
        """Convenience: the ranked list of result ids (top-to-bottom)."""
        payload = self.recall(query, **flags)
        return [res.get("id") or "" for res in payload.get("results", []) if res.get("id")]


@pytest.fixture
def behavioral_kb(tmp_path: Path) -> BehavioralKB:
    """A fresh hermetic real-engine KB, isolated per test via tmp_path."""
    return BehavioralKB(tmp_path)
