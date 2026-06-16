# ABOUTME: Recall eval harness — builds a hermetic KB from fixtures, runs golden
# ABOUTME: queries through the real recall pipeline, scores R@5/MRR/noise/latency/per-arm.
"""Eval harness for the reflect-kb recall pipeline.

Hermetic mode (default):
  - GLOBAL_LEARNINGS_PATH  -> tmp KB built from tests/eval/fixtures/corpus/
  - REFLECT_STATE_DIR      -> tmp state (recall cache, logs)
  - XDG_CACHE_HOME         -> tmp (so the qmd arm sees an empty index and recall
                              degrades to engine-only — documented, measured)

Live-smoke mode (--live):
  - No KB build; queries run against the user's real ~/.learnings + qmd index.
  - Report-only; numbers are non-deterministic by nature.

Metrics per query class and overall:
  - R@5                 any relevant doc in top-5
  - MRR                 1/rank of first relevant
  - inject-noise-rate   non-OOD: top-3 has zero relevant.  OOD: top-3 has ANY result.
  - latency p50/p95     wall-clock per recall invocation
  - per-arm contribution  which arm (graphrag/qmd) could have produced each top-5 hit
"""
from __future__ import annotations

import json
import os
import shutil
import statistics
import subprocess
import time
from dataclasses import dataclass, field
from pathlib import Path

import yaml

HERE = Path(__file__).parent
FIXTURES = HERE / "fixtures"
CORPUS = FIXTURES / "corpus"
GOLDEN = FIXTURES / "golden_queries.yaml"

# HERE = <repo>/reflect-kb/tests/eval — parents[1] is reflect-kb/, parents[2]
# is the repo root where plugins/ lives alongside reflect-kb/.
_CANDIDATES = [
    HERE.parents[2] / "plugins" / "reflect" / "skills" / "recall" / "scripts" / "recall.py",
    # standalone reflect-kb checkout with the plugin as a sibling dir
    HERE.parents[1].parent / "plugins" / "reflect" / "skills" / "recall" / "scripts" / "recall.py",
]
RECALL_PY = next((p for p in _CANDIDATES if p.exists()), _CANDIDATES[0])
if not RECALL_PY.exists():
    raise RuntimeError(f"recall.py not found; tried: {[str(p) for p in _CANDIDATES]}")

TOP_K = 5
INJECT_K = 3  # SessionStart injects top-3


class HarnessError(RuntimeError):
    pass


@dataclass
class QueryResult:
    qid: str
    qclass: str
    query: str
    relevant: list[str]
    returned_ids: list[str]
    latency_s: float
    arm_hits: dict[str, list[str]] = field(default_factory=dict)

    @property
    def first_relevant_rank(self) -> int | None:
        for i, rid in enumerate(self.returned_ids, start=1):
            if rid in self.relevant:
                return i
        return None

    @property
    def recall_at_5(self) -> float | None:
        if not self.relevant:  # OOD — R@5 undefined
            return None
        top = self.returned_ids[:TOP_K]
        return 1.0 if any(r in top for r in self.relevant) else 0.0

    @property
    def mrr(self) -> float | None:
        if not self.relevant:
            return None
        r = self.first_relevant_rank
        return (1.0 / r) if (r and r <= TOP_K) else 0.0

    @property
    def is_noise(self) -> bool:
        top = self.returned_ids[:INJECT_K]
        if not self.relevant:  # OOD: anything returned is noise
            return len(top) > 0
        return not any(r in top for r in self.relevant)


def _run(cmd: list[str], env: dict, timeout: int = 600) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, capture_output=True, text=True, env=env, timeout=timeout)


class EvalHarness:
    def __init__(self, workdir: Path, live: bool = False, debug: bool = False):
        self.workdir = Path(workdir)
        self.live = live
        self.debug = debug
        self.kb_dir = self.workdir / "kb"
        self.state_dir = self.workdir / "state"
        self.cache_home = self.workdir / "xdg-cache"

    # ---------- environment ----------
    def env(self) -> dict:
        env = dict(os.environ)
        if not self.live:
            env["GLOBAL_LEARNINGS_PATH"] = str(self.kb_dir)
            env["REFLECT_STATE_DIR"] = str(self.state_dir)
            # Isolate the qmd index (reads XDG_CACHE_HOME) so the BM25 arm sees
            # an empty collection instead of the user's live one.
            env["XDG_CACHE_HOME"] = str(self.cache_home)
            # ...but pin the HF model caches back to the real home cache, or the
            # XDG override forces a ~420MB model re-download into the tmp dir.
            env.setdefault("HF_HOME", str(Path.home() / ".cache" / "huggingface"))
            env.setdefault(
                "SENTENCE_TRANSFORMERS_HOME",
                str(Path.home() / ".cache" / "torch" / "sentence_transformers"),
            )
        # RECALL_EVAL_BIN_DIR: a venv bin dir holding a full-stack `reflect`
        # (the global install may be the slim build without [graph]). Prepended
        # so both the harness and recall.py's subprocess resolve the same CLI.
        bin_dir = os.environ.get("RECALL_EVAL_BIN_DIR")
        if bin_dir:
            env["PATH"] = f"{bin_dir}:{env.get('PATH', '')}"
        if self.debug:
            env["REFLECT_RECALL_DEBUG"] = "1"
        return env

    # ---------- KB build ----------
    def build_kb(self) -> None:
        if self.live:
            return
        if not shutil.which("reflect"):
            raise HarnessError("`reflect` CLI not on PATH — install reflect-kb (pipx install .[graph])")
        for d in (self.kb_dir, self.state_dir, self.cache_home):
            d.mkdir(parents=True, exist_ok=True)
        r = _run(["reflect", "init"], self.env())
        if r.returncode != 0:
            raise HarnessError(f"reflect init failed: {r.stderr[-500:]}")
        docs_dir = self.kb_dir / "documents"
        docs_dir.mkdir(exist_ok=True)
        n = 0
        for md in sorted(CORPUS.glob("*.md")):
            shutil.copy2(md, docs_dir / md.name)
            sidecar = md.with_suffix("").with_suffix(".entities.yaml")
            # md path like x.md -> sidecar x.entities.yaml
            sidecar = CORPUS / (md.stem + ".entities.yaml")
            if sidecar.exists():
                shutil.copy2(sidecar, docs_dir / sidecar.name)
            n += 1
        r = _run(["reflect", "reindex", "--force"], self.env(), timeout=1800)
        if r.returncode != 0:
            raise HarnessError(f"reflect reindex failed: {r.stderr[-800:]}")
        if self.debug:
            print(f"[harness] indexed {n} docs into {self.kb_dir}")

    # ---------- query ----------
    def run_query(self, query: str) -> tuple[list[str], float]:
        t0 = time.perf_counter()
        extra = os.environ.get("RECALL_EVAL_EXTRA", "").split()
        r = _run(
            ["python3", str(RECALL_PY), query,
             "--limit", str(TOP_K), "--format", "json", "--no-cache",
             "--confidence", "ANY", *extra],
            self.env(), timeout=300,
        )
        dt = time.perf_counter() - t0
        if r.returncode != 0:
            return [], dt
        try:
            payload = json.loads(r.stdout or "{}")
        except json.JSONDecodeError:
            return [], dt
        ids = [res.get("id") or "" for res in payload.get("results", [])]
        return [i for i in ids if i], dt

    def arm_contribution(self, query: str) -> dict[str, list[str]]:
        """Which top-5 ids each arm produces on its own (diagnostic)."""
        arms: dict[str, list[str]] = {}
        # graphrag arm
        r = _run(["reflect", "search", query, "--format", "json", "--limit", str(TOP_K)],
                 self.env(), timeout=300)
        ids: list[str] = []
        if r.returncode == 0:
            try:
                ctx = json.loads(r.stdout).get("context", "")
                for chunk in ctx.split("--New Chunk--"):
                    for line in chunk.splitlines():
                        if line.strip().startswith("name:"):
                            ids.append(line.split(":", 1)[1].strip())
                            break
            except (json.JSONDecodeError, AttributeError):
                pass
        arms["graphrag"] = ids[:TOP_K]
        # qmd arm
        qmd = shutil.which("qmd")
        if qmd:
            r = _run([qmd, "search", query, "-c", "learnings", "--limit", str(TOP_K)],
                     self.env(), timeout=60)
            qids = []
            if r.returncode == 0:
                for line in r.stdout.splitlines():
                    if "qmd://learnings/" in line:
                        frag = line.split("qmd://learnings/", 1)[1].split()[0]
                        qids.append(Path(frag).stem)
            arms["qmd"] = qids[:TOP_K]
        else:
            arms["qmd"] = []
        return arms

    # ---------- full run ----------
    def run(self, with_arms: bool = True) -> dict:
        queries = yaml.safe_load(GOLDEN.read_text())["queries"]
        results: list[QueryResult] = []
        for q in queries:
            ids, dt = self.run_query(q["query"])
            qr = QueryResult(
                qid=q["id"], qclass=q["class"], query=q["query"],
                relevant=q.get("relevant", []), returned_ids=ids, latency_s=dt,
            )
            if with_arms:
                qr.arm_hits = self.arm_contribution(q["query"])
            results.append(qr)
            if self.debug:
                print(f"[harness] {q['id']:9} {dt:6.2f}s  top={ids[:3]}")
        return self.score(results)

    # ---------- scoring ----------
    @staticmethod
    def score(results: list[QueryResult]) -> dict:
        def mean(xs):
            xs = [x for x in xs if x is not None]
            return round(sum(xs) / len(xs), 4) if xs else None

        by_class: dict[str, list[QueryResult]] = {}
        for r in results:
            by_class.setdefault(r.qclass, []).append(r)

        lat = sorted(r.latency_s for r in results)
        def pct(p):
            if not lat:
                return None
            idx = min(len(lat) - 1, max(0, int(round(p / 100 * (len(lat) + 1))) - 1))
            return round(lat[idx], 3)

        per_class = {}
        for cls, rs in sorted(by_class.items()):
            per_class[cls] = {
                "n": len(rs),
                "recall_at_5": mean([r.recall_at_5 for r in rs]),
                "mrr": mean([r.mrr for r in rs]),
                "noise_rate": mean([1.0 if r.is_noise else 0.0 for r in rs]),
            }

        arm_stats = {"graphrag_only": 0, "qmd_only": 0, "both": 0, "neither": 0}
        for r in results:
            if not r.arm_hits:
                continue
            g = set(r.arm_hits.get("graphrag", []))
            qm = set(r.arm_hits.get("qmd", []))
            for rid in r.returned_ids[:TOP_K]:
                in_g, in_q = rid in g, rid in qm
                if in_g and in_q:
                    arm_stats["both"] += 1
                elif in_g:
                    arm_stats["graphrag_only"] += 1
                elif in_q:
                    arm_stats["qmd_only"] += 1
                else:
                    arm_stats["neither"] += 1

        return {
            "overall": {
                "n_queries": len(results),
                "recall_at_5": mean([r.recall_at_5 for r in results]),
                "mrr": mean([r.mrr for r in results]),
                "noise_rate": mean([1.0 if r.is_noise else 0.0 for r in results]),
                "latency_p50_s": pct(50),
                "latency_p95_s": pct(95),
            },
            "per_class": per_class,
            "per_arm_top5_attribution": arm_stats,
            "queries": [
                {
                    "id": r.qid, "class": r.qclass,
                    "returned": r.returned_ids[:TOP_K],
                    "relevant": r.relevant,
                    "r_at_5": r.recall_at_5, "mrr": r.mrr, "noise": r.is_noise,
                    "latency_s": round(r.latency_s, 3),
                }
                for r in results
            ],
        }
