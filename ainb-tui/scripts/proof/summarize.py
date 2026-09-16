#!/usr/bin/env python3
"""Fold proof-out/<node>/result.json files into summary.json and summary.md.

Usage: summarize.py <proof-out dir> <binary version line> [source commit]

Exit status is 1 when any node failed or a result is missing or unreadable,
so run.sh can hand it straight back as its own.
"""

import json
import sys
from pathlib import Path

ORDER_FILE = "order.txt"


def load_results(out: Path) -> list[dict]:
    order = []
    order_path = out / ORDER_FILE
    if order_path.exists():
        order = [line.strip() for line in order_path.read_text().splitlines() if line.strip()]
    nodes = sorted(p.name for p in out.iterdir() if p.is_dir())
    # Every node the run meant to execute is ranked, directory or not: one
    # that died before writing anything becomes a synthetic failure below.
    ranked = order + [n for n in nodes if n not in order]
    results = []
    for node in ranked:
        path = out / node / "result.json"
        try:
            results.append(json.loads(path.read_text()))
        except (OSError, json.JSONDecodeError) as err:
            results.append({
                "node": node,
                "expected": "",
                "observed": [f"no readable result.json: {err}"],
                "pass": False,
                "skipped": None,
                "issue": None,
                "capture": [],
                "binary": "",
                "started_at": "",
            })
    return results


def md_cell(text: str) -> str:
    return text.replace("|", "\\|").replace("\n", " ")


def render_md(results: list[dict], binary: str, source: str) -> str:
    passed = sum(1 for r in results if r["pass"] and not r.get("skipped"))
    skipped = sum(1 for r in results if r.get("skipped"))
    lines = [
        "# Proof run",
        "",
        f"**Binary:** `{binary}`",
        "",
    ]
    if source:
        lines += [f"**Source:** `{source}` (the checkout run.sh ran from)", ""]
    lines += [
        f"**Result:** {passed} of {len(results)} nodes pass"
        + (f", {skipped} skipped." if skipped else "."),
        "",
        "| node | result | expected | observed | captures |",
        "|---|---|---|---|---|",
    ]
    for r in results:
        verdict = "SKIP" if r.get("skipped") else "PASS" if r["pass"] else "**FAIL**"
        if not r["pass"] and r.get("issue"):
            verdict += f" #{r['issue']}"
        failed = [o for o in r["observed"] if o.startswith("FAILED")]
        shown = failed if failed else r["observed"]
        observed = "<br>".join(md_cell(o) for o in shown)
        captures = ", ".join(
            f"`{c}`" for c in r["capture"] if c.endswith(".txt")
        )
        lines.append(
            f"| `{r['node']}` | {verdict} | {md_cell(r['expected'])} | {observed} | {captures} |"
        )
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    out = Path(sys.argv[1])
    binary = sys.argv[2]
    source = sys.argv[3] if len(sys.argv) > 3 else ""
    results = load_results(out)
    summary = {
        "binary": binary,
        "source": source,
        "nodes": len(results),
        "passed": sum(1 for r in results if r["pass"] and not r.get("skipped")),
        "skipped": [r["node"] for r in results if r.get("skipped")],
        "failed": [r["node"] for r in results if not r["pass"]],
        "results": results,
    }
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    (out / "summary.md").write_text(render_md(results, binary, source))
    skipped = f"; skipped: {', '.join(summary['skipped'])}" if summary["skipped"] else ""
    print(f"proof: {summary['passed']} of {summary['nodes']} nodes pass; "
          f"failed: {', '.join(summary['failed']) or 'none'}{skipped}")
    print(f"proof: {out / 'summary.md'}")
    return 0 if not summary["failed"] and results else 1


if __name__ == "__main__":
    sys.exit(main())
