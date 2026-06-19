# ABOUTME: Judge calibration — re-grade cached predictions with haiku/sonnet/opus.
# ABOUTME: Isolates the JUDGE effect (reuses answers); reports J per judge + agreement vs opus.
"""Answers the question "can we trust a cheaper judge?" empirically.

Loads already-cached (question, gold, predicted, category) verdicts for a tag,
re-runs ONLY the judge leg with each model, and reports per-judge overall/per-cat
J plus pairwise agreement against the opus judge (the reference).

Usage: python3 calibrate_judge.py --tag pilot50_v2 --judges haiku,sonnet,opus
"""
from __future__ import annotations

import argparse
import asyncio
import json
from pathlib import Path

import locomo_bench as lb  # reuse claude(), judge_prompt, JUDGE_SYS, CACHE

NAME2CAT = {v: k for k, v in lb.CATEGORY.items()}


def load_preds(tag: str) -> list[dict]:
    root = lb.CACHE / "qa" / tag
    out = []
    for f in sorted(root.rglob("*.json")):
        d = json.loads(f.read_text())
        if "predicted" in d and "gold" in d:
            d["_cat"] = NAME2CAT.get(d.get("category", ""), 0)
            out.append(d)
    return out


async def judge_one(pred: dict, model: str, sem) -> bool:
    out = await lb.claude(
        lb.judge_prompt(pred["question"], pred["gold"], pred["predicted"], pred["_cat"]),
        lb.JUDGE_SYS, sem, model=model)
    import re
    m = re.search(r"\{.*\}", out.text, re.S)
    if m:
        try:
            return bool(json.loads(m.group(0)).get("correct"))
        except json.JSONDecodeError:
            pass
    return False


async def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--tag", default="pilot50_v2")
    ap.add_argument("--judges", default="haiku,sonnet,opus")
    ap.add_argument("--concurrency", type=int, default=6)
    a = ap.parse_args()
    judges = a.judges.split(",")
    preds = load_preds(a.tag)
    print(f"loaded {len(preds)} cached predictions for tag={a.tag}")
    sem = asyncio.Semaphore(a.concurrency)

    # grade[model][i] = bool
    grade: dict[str, list[bool]] = {}
    for model in judges:
        grade[model] = await asyncio.gather(*[judge_one(p, model, sem) for p in preds])
        j = sum(grade[model]) / len(preds)
        print(f"  {model:8} overall J = {j:.3f}")

    cats = ["single_hop", "multi_hop", "temporal", "open_domain", "adversarial"]
    print("\nper-category J:")
    print(f"{'judge':8}" + "".join(f"{c[:9]:>11}" for c in cats))
    for model in judges:
        row = f"{model:8}"
        for c in cats:
            idx = [i for i, p in enumerate(preds) if p.get("category") == c]
            row += f"{(sum(grade[model][i] for i in idx)/len(idx) if idx else 0):>11.3f}"
        print(row)

    if "opus" in judges:
        ref = grade["opus"]
        print("\nagreement vs OPUS (reference judge):")
        for model in judges:
            if model == "opus":
                continue
            agree = sum(1 for i in range(len(preds)) if grade[model][i] == ref[i]) / len(preds)
            # confusion: where they differ
            fp = sum(1 for i in range(len(preds)) if grade[model][i] and not ref[i])
            fn = sum(1 for i in range(len(preds)) if not grade[model][i] and ref[i])
            print(f"  {model:8} agree={agree:.3f}  (judged-correct-but-opus-no={fp}, "
                  f"judged-wrong-but-opus-yes={fn})")

    out = lb.RESULTS / f"judge_calibration_{a.tag}.json"
    out.write_text(json.dumps(
        {"tag": a.tag, "n": len(preds), "judges": judges,
         "overall_J": {m: sum(grade[m]) / len(preds) for m in judges}}, indent=2))
    print(f"\nwrote {out}")


if __name__ == "__main__":
    asyncio.run(main())
