# ABOUTME: Regression tests for Wave-1 retrieval ports — R4 token-budget, R7 OOD gate, R1 graph arm.
# ABOUTME: Pure-unit where possible; the graph-arm fan-out is pinned via a fake `reflect` CLI on PATH.
"""Ports R4 / R7 / R1 in recall.py.

R4: --max-tokens bounds results by estimated tokens (≥1 always kept).
R7: --min-overlap suppresses out-of-domain result sets (top-hit query-term
    coverage below threshold → empty + ood_gated marker).
R1: a third parallel arm (`reflect search --mode local`) joins the RRF fusion;
    disabled via RECALL_GRAPH_ARM=0; never fans out twice for --mode local.
"""

from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
RECALL = PLUGIN_ROOT / "skills" / "recall" / "scripts" / "recall.py"
sys.path.insert(0, str(RECALL.parent))

import importlib
recall_mod = importlib.import_module("recall")
from recall import (  # noqa: E402
    Learning,
    apply_ood_gate,
    filter_by_token_budget,
    lexical_overlap,
)


def _lrn(text: str, name: str = "doc", confidence: str = "high") -> Learning:
    return Learning(chunk_text=text, frontmatter={"name": name, "confidence": confidence})


# ---------- R4: token budget ----------

def test_budget_zero_means_unbounded():
    ls = [_lrn("x" * 400, f"d{i}") for i in range(5)]
    assert filter_by_token_budget(ls, 0) == ls


def test_budget_cuts_at_estimate():
    # each ~100 tokens (400 chars); budget 250 → 2 fit, 3rd would exceed
    ls = [_lrn("x" * 400, f"d{i}") for i in range(5)]
    out = filter_by_token_budget(ls, 250)
    assert len(out) == 2


def test_budget_always_keeps_first():
    big = _lrn("x" * 40_000, "big")  # ~10k tokens
    out = filter_by_token_budget([big], 100)
    assert out == [big], "a single long learning must not starve the caller"


def test_cli_max_tokens_flag_accepted():
    r = subprocess.run(
        [sys.executable, str(RECALL), "anything", "--max-tokens", "100",
         "--format", "json", "--no-cache"],
        capture_output=True, text=True, timeout=60,
        env={**os.environ, "PATH": "/usr/bin:/bin"},  # no reflect CLI → graceful empty
    )
    assert r.returncode == 0


# ---------- R7: OOD gate ----------

def test_overlap_high_for_matching_doc():
    lrn = _lrn("tmux kill-server destroys every session on the socket")
    assert lexical_overlap("tmux kill-server destroyed sessions", lrn) >= 0.5


def test_overlap_near_zero_for_ood():
    lrn = _lrn("sqlite WAL checkpoint starvation on long readers")
    assert lexical_overlap("istio service mesh sidecar injection", lrn) < 0.2


def test_hyphen_variants_count():
    lrn = _lrn("never run kill server on the shared tmux socket")
    # query says kill-server; doc says "kill server"
    assert lexical_overlap("tmux kill-server", lrn) == 1.0


def test_gate_suppresses_below_threshold():
    ls = [_lrn("completely unrelated content about cooking pasta")]
    out, gated = apply_ood_gate(ls, "istio sidecar injection", 0.2)
    assert gated and out == []


def test_gate_passes_relevant_sets():
    ls = [_lrn("istio sidecar injection is configured via the mesh webhook")]
    out, gated = apply_ood_gate(ls, "istio sidecar injection", 0.2)
    assert not gated and out == ls


def test_gate_off_at_zero():
    ls = [_lrn("junk")]
    out, gated = apply_ood_gate(ls, "istio sidecar injection", 0.0)
    assert not gated and out == ls


def test_vacuous_query_never_gates():
    ls = [_lrn("anything")]
    out, gated = apply_ood_gate(ls, "the of and", 0.9)
    assert not gated


# ---------- R1: graph arm fan-out (via fake CLI) ----------

@pytest.fixture()
def fake_reflect(tmp_path):
    """A fake `reflect` CLI that records each --mode it is called with and
    returns one distinct chunk per mode."""
    calls = tmp_path / "calls.log"
    script = tmp_path / "bin" / "reflect"
    script.parent.mkdir()
    script.write_text(f"""#!/usr/bin/env python3
import json, sys
mode = sys.argv[sys.argv.index("--mode") + 1] if "--mode" in sys.argv else "?"
with open({str(calls)!r}, "a") as f:
    f.write(mode + "\\n")
chunk = "---\\nname: from-" + mode + "\\nconfidence: high\\n---\\nbody " + mode
print(json.dumps({{"context": chunk}}))
""")
    script.chmod(script.stat().st_mode | stat.S_IEXEC)
    return script.parent, calls


def _run_recall(bin_dir: Path, tmp_path: Path, *args, env_extra=None):
    env = {
        **os.environ,
        "PATH": f"{bin_dir}:/usr/bin:/bin",
        "REFLECT_STATE_DIR": str(tmp_path / "state"),
        # R2 adds a `reflect rerank` call after fusion; disable it here so
        # the fake CLI's call log stays a pure record of the R1 search arms.
        "RECALL_CROSS_ENCODER": "0",
        **(env_extra or {}),
    }
    return subprocess.run(
        [sys.executable, str(RECALL), "redis pool exhaustion",
         "--format", "json", "--no-cache", *args],
        capture_output=True, text=True, timeout=60, env=env,
    )


def test_graph_arm_fans_out_naive_plus_local(fake_reflect, tmp_path):
    bin_dir, calls = fake_reflect
    r = _run_recall(bin_dir, tmp_path)
    assert r.returncode == 0, r.stderr
    modes = calls.read_text().split()
    assert "naive" in modes and "local" in modes, modes
    payload = json.loads(r.stdout)
    ids = [x["id"] for x in payload["results"]]
    assert "from-naive" in ids and "from-local" in ids  # both arms fused


def test_graph_arm_disabled_by_env(fake_reflect, tmp_path):
    bin_dir, calls = fake_reflect
    r = _run_recall(bin_dir, tmp_path, env_extra={"RECALL_GRAPH_ARM": "0"})
    assert r.returncode == 0
    modes = calls.read_text().split()
    assert modes == ["naive"], modes


def test_explicit_local_mode_does_not_fan_out_twice(fake_reflect, tmp_path):
    bin_dir, calls = fake_reflect
    r = _run_recall(bin_dir, tmp_path, "--mode", "local")
    assert r.returncode == 0
    modes = calls.read_text().split()
    assert modes == ["local"], modes


def test_graph_arm_failure_is_nonfatal(fake_reflect, tmp_path):
    """Booster contract: a broken local arm must not kill the result set."""
    bin_dir, calls = fake_reflect
    script = bin_dir / "reflect"
    body = script.read_text().replace(
        'print(json.dumps({"context": chunk}))',
        'import sys as s\n'
        'if mode == "local": s.exit(3)\n'
        'print(json.dumps({"context": chunk}))',
    )
    script.write_text(body)
    r = _run_recall(bin_dir, tmp_path)
    assert r.returncode == 0
    payload = json.loads(r.stdout)
    ids = [x["id"] for x in payload["results"]]
    assert "from-naive" in ids  # primary survived


# ---------- JSON surface ----------

def test_json_has_ood_gated_field(fake_reflect, tmp_path):
    bin_dir, _ = fake_reflect
    r = _run_recall(bin_dir, tmp_path, "--min-overlap", "0.99")
    payload = json.loads(r.stdout)
    assert payload["ood_gated"] is True
    assert payload["results"] == []


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
