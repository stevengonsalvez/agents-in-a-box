# ABOUTME: Render a LOCOMO benchmark report_*.json into a human REPORT.md scorecard.
# ABOUTME: Pure formatting — no LLM, no engine; reads the harness JSON and emits markdown.
"""Usage: python3 make_report.py results/report_pilot50.json [REPORT.md]"""
from __future__ import annotations

import json
import sys
from pathlib import Path

CATS = ["single_hop", "multi_hop", "temporal", "open_domain", "adversarial"]
CFG_LABEL = {
    "arms_on": "reflect 4.1.0 (arms ON)",
    "arms_off": "reflect (arms OFF ≈ 4.0)",
    "no_memory": "no-memory baseline",
    "full_context": "full-context baseline",
}


def micro(report: dict, cfg: str, cat: str | None = None) -> tuple[int, int]:
    c = n = 0
    for s in report["samples"]:
        sc = s["by_config"].get(cfg)
        if not sc:
            continue
        if cat is None:
            n += sc["n_qa"]; c += round(sc["j_score"] * sc["n_qa"])
        else:
            pc = sc["per_category"].get(cat)
            if pc:
                n += pc["n"]; c += round(pc["j_score"] * pc["n"])
    return c, n


def cell(c: int, n: int) -> str:
    return f"{c/n:.3f} ({c}/{n})" if n else "—"


def main() -> None:
    rpt = json.loads(Path(sys.argv[1]).read_text())
    out = Path(sys.argv[2]) if len(sys.argv) > 2 else Path(sys.argv[1]).parent / "REPORT.md"
    cfgs = rpt["configs"]
    samples = rpt["samples"]
    n_notes = sum(s["n_notes"] for s in samples)
    n_sel = samples[0].get("n_selected", samples[0]["by_config"][cfgs[0]]["n_qa"]) if samples else 0

    L: list[str] = []
    L.append("# LOCOMO benchmark — reflect 4.1.0 memory engine\n")
    L.append(f"- **Model (answer + judge + writer):** {rpt['model']} (via `claude -p`, clean settings)")
    L.append(f"- **Dataset:** LOCOMO ({len(samples)} conversation(s) of locomo10)")
    L.append(f"- **QA scored:** {sum(micro(rpt, cfgs[0])[1] for _ in [0])} per config "
             f"({n_sel} selected/sample, stratified by category)")
    L.append(f"- **Memory notes stored:** {n_notes}")
    L.append(f"- **Configs:** {', '.join(cfgs)}\n")

    # --- scorecard ---
    L.append("## J-score (LLM-judge correctness)\n")
    head = "| config | " + " | ".join(c.replace("_", "-") for c in CATS) + " | **overall** |"
    L.append(head)
    L.append("|" + "---|" * (len(CATS) + 2))
    for cfg in cfgs:
        row = [CFG_LABEL.get(cfg, cfg)]
        for cat in CATS:
            row.append(cell(*micro(rpt, cfg, cat)))
        oc, on = micro(rpt, cfg)
        row.append(f"**{oc/on:.3f}**" if on else "—")
        L.append("| " + " | ".join(row) + " |")
    L.append("")

    # --- ablation ---
    if "arms_on" in cfgs and "arms_off" in cfgs:
        on, off = micro(rpt, "arms_on"), micro(rpt, "arms_off")
        d = (on[0]/on[1] if on[1] else 0) - (off[0]/off[1] if off[1] else 0)
        L.append("## 4.1.0 recall-arms ablation\n")
        L.append(f"- arms ON overall J = **{on[0]/on[1]:.3f}** ({on[0]}/{on[1]})")
        L.append(f"- arms OFF overall J = **{off[0]/off[1]:.3f}** ({off[0]}/{off[1]})")
        L.append(f"- **Δ (on − off) = {d:+.3f}**  "
                 f"{'→ arms help' if d > 0 else '→ no measured gain' if d == 0 else '→ arms hurt (investigate)'}\n")
        L.append("Per-category Δ:\n")
        L.append("| category | arms ON | arms OFF | Δ |")
        L.append("|---|---|---|---|")
        for cat in CATS:
            o = micro(rpt, "arms_on", cat); f = micro(rpt, "arms_off", cat)
            if o[1] and f[1]:
                L.append(f"| {cat.replace('_','-')} | {o[0]/o[1]:.3f} | {f[0]/f[1]:.3f} "
                         f"| {o[0]/o[1]-f[0]/f[1]:+.3f} |")
        L.append("")

    # --- ops ---
    L.append("## Latency, tokens, cost\n")
    L.append("| config | recall p50 | recall p95 | answer p50 | answer p95 | tokens | cost |")
    L.append("|---|---|---|---|---|---|---|")
    for cfg in cfgs:
        agg = {"rp50": [], "rp95": [], "ap50": [], "ap95": [], "tok": 0, "cost": 0.0}
        for s in samples:
            sc = s["by_config"].get(cfg, {})
            for k, key in [("rp50", "recall_latency_p50_s"), ("rp95", "recall_latency_p95_s"),
                           ("ap50", "answer_latency_p50_s"), ("ap95", "answer_latency_p95_s")]:
                if sc.get(key) is not None:
                    agg[k].append(sc[key])
            agg["tok"] += sc.get("total_tokens", 0)
            agg["cost"] += sc.get("total_cost_usd", 0)

        def m(xs):
            return f"{sum(xs)/len(xs):.1f}s" if xs else "—"
        L.append(f"| {CFG_LABEL.get(cfg, cfg)} | {m(agg['rp50'])} | {m(agg['rp95'])} "
                 f"| {m(agg['ap50'])} | {m(agg['ap95'])} | {agg['tok']:,} | ${agg['cost']:.2f} |")
    L.append("")

    # --- methodology ---
    L.append("## Methodology notes\n")
    L.append("- **Retrieval** is reflect-kb's real engine (`reflect reindex` + `recall.py`); the "
             "57 v4.1.0 arms toggle via `RECALL_*` env knobs (arms-ON sets them, arms-OFF leaves "
             "pre-4.1 defaults).")
    L.append("- **Ingestion** is a LOCOMO-domain adapter: each session is LLM-extracted into atomic "
             "memory notes (reflect's shipped writer targets coding transcripts, not persona chat).")
    L.append("- **Answer/judge** run on clean `claude -p --setting-sources '' --strict-mcp-config` "
             "(no session hooks/CLAUDE.md/MCP — no caveman pollution).")
    L.append("- **J-score** = LLM-judge correctness; adversarial (cat 5) is correct only when the "
             "model abstains / says not-mentioned.")
    out.write_text("\n".join(L) + "\n")
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
