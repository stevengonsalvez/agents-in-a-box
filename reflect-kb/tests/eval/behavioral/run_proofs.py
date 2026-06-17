#!/usr/bin/env python3
# ABOUTME: Discovers behavioral/proofs/proof_*.py, runs them via pytest, and emits
# ABOUTME: a port x verdict matrix to results/matrix.json plus a markdown table to stdout.
"""Behavioral-proof matrix runner.

Runs every proof under behavioral/proofs/ — one pytest invocation per file so a
crash in one proof can't poison the others — collects each file's verdict from
pytest's return code, writes results/matrix.json, and prints a markdown table.
Dependency-free: no pytest plugins required (return code 0 = pass, 1 = test
failure, anything else = collection/internal error).

The port id is taken from the filename: proof_<PORT>_<slug>.py -> <PORT>.
A single file may hold several test functions (e.g. R8 base + R8 bounded); the
file passes only if every test in it passes (pytest returns 0).

Usage:
    # from reflect-kb/ (so the dev extras / venv resolve):
    RECALL_EVAL_BIN_DIR=/tmp/recall-eval-venv/bin \
      uv run --extra dev python tests/eval/behavioral/run_proofs.py

    # run a subset:
    ... run_proofs.py R7 R8        # only proofs whose port matches
"""
from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).parent
PROOFS_DIR = HERE / "proofs"
RESULTS_DIR = HERE / "results"
MATRIX_JSON = RESULTS_DIR / "matrix.json"

_PORT_RE = re.compile(r"^proof_([A-Za-z]+\d+)_")


def _port_of(path: Path) -> str:
    m = _PORT_RE.match(path.name)
    return m.group(1) if m else path.stem


def discover(filters: list[str]) -> list[Path]:
    proofs = sorted(p for p in PROOFS_DIR.glob("proof_*.py") if p.name != "__init__.py")
    if filters:
        wanted = {f.upper() for f in filters}
        proofs = [p for p in proofs if _port_of(p).upper() in wanted]
    return proofs


def run(proofs: list[Path]) -> dict:
    """Run each proof file under its own pytest; fold into a port matrix."""
    ports: dict[str, dict] = {}
    for proof in proofs:
        port = _port_of(proof)
        cmd = [
            sys.executable, "-m", "pytest", str(proof),
            "-q", "-p", "no:cacheprovider",
        ]
        proc = subprocess.run(cmd, capture_output=True, text=True)
        sys.stderr.write(f"\n===== {port} ({proof.name}) =====\n")
        sys.stderr.write(proc.stdout)
        sys.stderr.write(proc.stderr)
        # pytest exit codes: 0 all passed, 1 tests failed, 2 interrupted,
        # 3 internal error, 4 usage, 5 no tests collected.
        if proc.returncode == 0:
            verdict = "PASS"
        elif proc.returncode == 1:
            verdict = "FAIL"
        else:
            verdict = "ERROR"
        ports[port] = {
            "file": proof.name,
            "verdict": verdict,
            "returncode": proc.returncode,
        }

    verdicts = [e["verdict"] for e in ports.values()]
    summary = {
        "total": len(ports),
        "passed": verdicts.count("PASS"),
        "failed": verdicts.count("FAIL"),
        "errored": verdicts.count("ERROR"),
    }
    return {"ports": ports, "summary": summary}


def render_markdown(matrix: dict) -> str:
    rows = ["| Port | Verdict | Proof file |", "| --- | --- | --- |"]
    for port in sorted(matrix["ports"]):
        e = matrix["ports"][port]
        rows.append(f"| {port} | {e['verdict']} | `{e['file']}` |")
    s = matrix["summary"]
    rows.append("")
    rows.append(
        f"**{s['passed']}/{s['total']} ports PASS** "
        f"(fail={s['failed']}, error={s['errored']})"
    )
    return "\n".join(rows)


def main(argv: list[str]) -> int:
    RESULTS_DIR.mkdir(parents=True, exist_ok=True)
    proofs = discover(argv)
    if not proofs:
        print("no proofs matched", file=sys.stderr)
        MATRIX_JSON.write_text(json.dumps({"ports": {}, "summary": {}}, indent=2))
        return 1
    matrix = run(proofs)
    MATRIX_JSON.write_text(json.dumps(matrix, indent=2))
    print(render_markdown(matrix))
    return 0 if matrix["summary"].get("failed", 0) == 0 and matrix["summary"].get("errored", 0) == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
