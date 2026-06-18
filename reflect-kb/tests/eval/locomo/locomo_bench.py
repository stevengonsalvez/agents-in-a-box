# ABOUTME: LOCOMO long-term-conversational-memory benchmark for the reflect 4.1.0 KB.
# ABOUTME: Ingests dialogue -> reflect-kb store -> recall -> Sonnet answer -> Sonnet judge (J-score), across 4 configs.
"""LOCOMO benchmark harness for reflect-kb (the reflect 4.1.0 memory engine).

Pipeline (per conversation sample):

  sessions ─▶ extract atomic memory notes (claude -p sonnet, clean)
           ─▶ write learning .md into a hermetic KB ─▶ reflect reindex
  question ─▶ recall (reflect-kb real engine) ─▶ context
           ─▶ answer (sonnet) ─▶ judge vs gold (sonnet, J-score)

Four configs (the ablation matrix):
  arms_on      reflect 4.1.0 recall arms ON  (RECALL_* env knobs set)
  arms_off     arms OFF (pre-4.1 / 4.0 default behavior)
  no_memory    answerer gets only the question (base-model floor)
  full_context full conversation stuffed into the prompt (upper bound)

Only the RETRIEVAL path is the real reflect-kb engine. The dialogue->note
extraction is a documented LOCOMO-domain ingestion adapter (reflect's shipped
writer is tuned for coding transcripts, not persona chat).

LLM path: `claude -p --setting-sources '' --strict-mcp-config` — clean Sonnet
with no session hooks/CLAUDE.md/MCP (no caveman pollution, ~$0.006/short call),
OAuth intact. No ANTHROPIC_API_KEY required.

Resumable: every extraction and every QA verdict is cached to results/cache/.
Re-running skips completed work.

Usage:
  python3 locomo_bench.py --samples 0                  # pilot: 1 convo, all configs
  python3 locomo_bench.py --samples 0 --limit-qa 6     # smoke: 6 QA
  python3 locomo_bench.py --samples all                # full locomo10
  python3 locomo_bench.py --samples 0 --configs arms_on,arms_off
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import re
import shutil
import statistics
import subprocess
import time
from dataclasses import dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parent
DATA = HERE / "data" / "locomo10.json"
RESULTS = HERE / "results"
CACHE = RESULTS / "cache"
KB_ROOT = RESULTS / "kb"

# repo root = reflect-kb/tests/eval/locomo -> parents[3]
REPO = HERE.parents[3]
RECALL_PY = REPO / "plugins" / "reflect" / "skills" / "recall" / "scripts" / "recall.py"
# Dedicated venv holding THIS worktree's reflect-kb[graph] (the global `reflect`
# is a stale namespace-shim install missing reflect_kb.cli.main). Both our direct
# `reflect` calls and recall.py's `reflect` subprocess resolve to it via PATH.
REFLECT_VENV = HERE.parents[2] / ".venv-locomo"  # reflect-kb/.venv-locomo
REFLECT_BIN_DIR = REFLECT_VENV / "bin"

MODEL = "sonnet"
CONCURRENCY = 8
RECALL_LIMIT = 8
RECALL_MAX_CHARS = 3000

CATEGORY = {1: "multi_hop", 2: "temporal", 3: "open_domain", 4: "single_hop", 5: "adversarial"}

# reflect 4.1.0 recall arms — set => ON (CHANGELOG: all gate OFF by default).
ARMS_ON_ENV = {
    "RECALL_GRAPH_ARM": "1",
    "RECALL_CROSS_ENCODER": "1",
    "RECALL_MMR": "1",
    "RECALL_TEMPORAL": "1",
    "RECALL_TEMPORAL_ARM": "1",
    "RECALL_BITEMPORAL_EDGES": "1",
    "RECALL_FUZZY_CACHE": "1",
    "RECALL_FOLLOWUP": "1",
    "REFLECT_TIERED_INJECT": "1",
}

ANSWER_SYS = (
    "You are a precise question-answering assistant for a long-term-memory benchmark. "
    "You are given MEMORY excerpts recalled from two friends' past conversations, then a QUESTION. "
    "Answer ONLY from the MEMORY. Be as short as possible: a name, a date, a place, a short phrase. "
    "If the MEMORY does not contain the answer, reply exactly: NOT MENTIONED."
)
JUDGE_SYS = (
    "You are a strict grader for a QA benchmark. Decide if the PREDICTED answer is correct "
    "given the GOLD answer. Accept paraphrases, equivalent dates, and supersets that contain the "
    "gold fact. Reject wrong, empty, or hallucinated answers. "
    'Reply with JSON only: {"correct": true|false, "reason": "<=12 words"}.'
)


# ----------------------------- LLM ---------------------------------------

@dataclass
class LLMOut:
    text: str
    in_tok: int
    out_tok: int
    cache_tok: int
    cost: float
    err: bool


async def claude(prompt: str, system: str, sem: asyncio.Semaphore,
                 model: str = MODEL, retries: int = 2) -> LLMOut:
    cmd = ["claude", "-p", prompt, "--model", model, "--setting-sources", "",
           "--strict-mcp-config", "--output-format", "json", "--system-prompt", system]
    async with sem:
        for attempt in range(retries + 1):
            try:
                proc = await asyncio.create_subprocess_exec(
                    *cmd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
                out, errb = await asyncio.wait_for(proc.communicate(), timeout=180)
            except (asyncio.TimeoutError, Exception):  # noqa: BLE001
                if attempt < retries:
                    await asyncio.sleep(2 * (attempt + 1))
                    continue
                return LLMOut("", 0, 0, 0, 0.0, True)
            try:
                d = json.loads(out.decode())
            except json.JSONDecodeError:
                if attempt < retries:
                    await asyncio.sleep(2)
                    continue
                return LLMOut("", 0, 0, 0, 0.0, True)
            u = d.get("usage", {}) or {}
            res = LLMOut(
                text=(d.get("result") or "").strip(),
                in_tok=u.get("input_tokens", 0) or 0,
                out_tok=u.get("output_tokens", 0) or 0,
                cache_tok=u.get("cache_creation_input_tokens", 0) or 0,
                cost=d.get("total_cost_usd") or 0.0,
                err=bool(d.get("is_error")),
            )
            if res.err and "logged in" in res.text.lower():
                raise SystemExit("claude not logged in — run /login")
            if res.err and attempt < retries:
                await asyncio.sleep(2)
                continue
            return res
    return LLMOut("", 0, 0, 0, 0.0, True)


# ----------------------------- data --------------------------------------

def sessions_sorted(conv: dict) -> list[tuple[str, str, list]]:
    """Return [(session_key, datetime, turns)] in numeric session order."""
    out = []
    for k, v in conv.items():
        m = re.fullmatch(r"session_(\d+)", k)
        if not m:
            continue
        n = int(m.group(1))
        dt = conv.get(f"session_{n}_date_time", "")
        out.append((n, k, dt, v))
    out.sort(key=lambda x: x[0])
    return [(k, dt, turns) for _n, k, dt, turns in out]


def turns_text(turns: list, dt: str) -> str:
    lines = [f"[{dt}]"]
    for t in turns:
        spk = t.get("speaker", "?")
        txt = t.get("text", "")
        cap = t.get("blip_caption")
        if cap:
            txt = f"{txt} (shared an image: {cap})" if txt else f"(shared an image: {cap})"
        lines.append(f"{spk}: {txt}")
    return "\n".join(lines)


def full_conversation(conv: dict) -> str:
    return "\n\n".join(turns_text(turns, dt) for _k, dt, turns in sessions_sorted(conv))


# ----------------------------- ingest ------------------------------------

EXTRACT_SYS = (
    "You extract EVERY durable fact from a chat between two friends, for a memory system "
    "that will later answer specific questions about who did what, when, where, and with "
    "whom. Be exhaustive and literal: one JSON item per CONCRETE fact — each event, date, "
    "place, name, number, object, purchase, preference, plan, relationship, feeling, or "
    "decision actually mentioned. Do NOT summarize or merge facts; prefer many specific "
    "facts over a few general ones. Preserve exact details verbatim (exact dates, proper "
    "nouns, quantities, titles, brand/place names). "
    'Each item: {"fact": "<one self-contained sentence: WHO did WHAT, WHEN/WHERE, with the '
    'specific detail>", "speaker": "<name>"}. Resolve every pronoun to a name. Put the '
    "session date into any time-relevant fact. Return ONLY a JSON array (no prose), with as "
    "many items as the conversation supports."
)


def _slug(s: str, n: int = 40) -> str:
    s = re.sub(r"[^a-z0-9]+", "-", s.lower()).strip("-")
    return s[:n] or "fact"


async def extract_session(sample_id: str, skey: str, dt: str, turns: list,
                          sem: asyncio.Semaphore) -> list[dict]:
    cdir = CACHE / "extract" / sample_id
    cdir.mkdir(parents=True, exist_ok=True)
    cf = cdir / f"{skey}.json"
    if cf.exists():
        cached = json.loads(cf.read_text())
        return cached["facts"] if isinstance(cached, dict) else cached
    prompt = f"Session date: {dt}\n\nConversation:\n{turns_text(turns, dt)}\n\nExtract the memory facts as a JSON array."
    out = await claude(prompt, EXTRACT_SYS, sem)
    facts: list[dict] = []
    if out.text:
        m = re.search(r"\[.*\]", out.text, re.S)
        if m:
            try:
                raw = json.loads(m.group(0))
                for it in raw:
                    if isinstance(it, dict) and it.get("fact"):
                        facts.append({"fact": str(it["fact"]),
                                      "speaker": str(it.get("speaker", "")),
                                      "session": skey, "date": dt})
            except json.JSONDecodeError:
                pass
    rec = {"facts": facts, "tokens": [out.in_tok, out.out_tok, out.cache_tok], "cost": out.cost}
    cf.write_text(json.dumps(rec))
    return facts  # type: ignore[return-value]


def write_notes(kb: Path, facts: list[dict]) -> int:
    docs = kb / "documents"
    docs.mkdir(parents=True, exist_ok=True)
    for i, f in enumerate(facts):
        date = (f.get("date") or "")[:10] or "2023-01-01"
        # normalize date to YYYY-MM-DD if it looks like a locomo datetime
        name = f"{f.get('session','s')}-{i:03d}-{_slug(f['fact'])}"
        body = f["fact"]
        spk = f.get("speaker", "")
        md = (
            f"---\nname: {name}\ntitle: {body[:70]}\ncategory: conversation\n"
            f"tags: [locomo, {_slug(spk, 20) or 'memory'}]\n"
            f"created: {date}\nspeaker: {spk}\n---\n\n{body}\n"
        )
        (docs / f"{name}.md").write_text(md)
    return len(facts)


def base_env(kb: Path, state: Path) -> dict:
    env = dict(os.environ)
    env["GLOBAL_LEARNINGS_PATH"] = str(kb)
    env["REFLECT_STATE_DIR"] = str(state)
    env["XDG_CACHE_HOME"] = str(state / "xdg")
    env.setdefault("HF_HOME", str(Path.home() / ".cache" / "huggingface"))
    env.setdefault("SENTENCE_TRANSFORMERS_HOME",
                   str(Path.home() / ".cache" / "torch" / "sentence_transformers"))
    # Prepend the local reflect-kb venv so `reflect` (here and inside recall.py)
    # resolves to this worktree's source, not the broken global install.
    if REFLECT_BIN_DIR.exists():
        env["PATH"] = f"{REFLECT_BIN_DIR}:{env.get('PATH', '')}"
        env["RECALL_EVAL_BIN_DIR"] = str(REFLECT_BIN_DIR)
    # strip any inherited arm knobs so arms_off is truly off
    for k in ARMS_ON_ENV:
        env.pop(k, None)
    return env


def reindex(kb: Path, state: Path) -> None:
    env = base_env(kb, state)
    for d in (kb, state, state / "xdg"):
        d.mkdir(parents=True, exist_ok=True)
    r = subprocess.run(["reflect", "init"], capture_output=True, text=True, env=env)
    if r.returncode != 0 and "exist" not in (r.stderr + r.stdout).lower():
        raise RuntimeError(f"reflect init failed: {r.stderr[-400:]}")
    r = subprocess.run(["reflect", "reindex", "--force"], capture_output=True, text=True,
                       env=env, timeout=1800)
    if r.returncode != 0:
        raise RuntimeError(f"reflect reindex failed: {r.stderr[-600:]}")


# ----------------------------- recall ------------------------------------

_RECALL_SEM: asyncio.Semaphore | None = None  # set in main_async — bounds torch RAM thrash
_RUN_TAG = "pilot"  # set in main_async — scopes the per-QA verdict cache per run/tuning
RECALL_TIMEOUT = 90  # recall normally 20-45s; kill hangs past this


async def recall_context(question: str, kb: Path, state: Path, arms_on: bool) -> tuple[str, float]:
    """Async recall via recall.py. Runs in its own process group so a hang
    (the engine occasionally wedges at 0% CPU on some queries) is killed with the
    whole subtree, not just the direct child. Bounded by _RECALL_SEM."""
    env = base_env(kb, state)
    if arms_on:
        env.update(ARMS_ON_ENV)
    cmd = ["python3", str(RECALL_PY), question, "--limit", str(RECALL_LIMIT),
           "--format", "markdown", "--max-chars", str(RECALL_MAX_CHARS),
           "--no-cache", "--confidence", "ANY"]
    assert _RECALL_SEM is not None
    t0 = time.perf_counter()
    async with _RECALL_SEM:
        proc = None
        try:
            proc = await asyncio.create_subprocess_exec(
                *cmd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL,
                env=env, start_new_session=True)
            try:
                out, _ = await asyncio.wait_for(proc.communicate(), timeout=RECALL_TIMEOUT)
                ctx = out.decode().strip() if proc.returncode == 0 else ""
            except asyncio.TimeoutError:
                _kill_tree(proc)
                ctx = ""
        except Exception:  # noqa: BLE001
            if proc:
                _kill_tree(proc)
            ctx = ""
    return ctx, time.perf_counter() - t0


def _kill_tree(proc) -> None:
    import os
    import signal
    try:
        os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        try:
            proc.kill()
        except ProcessLookupError:
            pass


# ----------------------------- QA ----------------------------------------

def gold_of(qa: dict) -> str:
    if qa.get("category") == 5:
        return "NOT MENTIONED (adversarial / not answerable from the conversation)"
    a = qa.get("answer")
    return str(a) if a is not None else "NOT MENTIONED"


def answer_prompt(question: str, context: str) -> str:
    if context:
        return f"MEMORY:\n{context}\n\nQUESTION: {question}\n\nAnswer:"
    return f"QUESTION: {question}\n\nAnswer:"


def judge_prompt(question: str, gold: str, pred: str, cat: int) -> str:
    extra = ""
    if cat == 5:
        extra = ("\nThis is an ADVERSARIAL question with no answer in the conversation. "
                 "PREDICTED is correct ONLY if it declines / says not mentioned / unknown.")
    return (f"QUESTION: {question}\nGOLD: {gold}\nPREDICTED: {pred}{extra}\n\n"
            'Is PREDICTED correct? JSON only: {"correct": bool, "reason": str}.')


@dataclass
class Verdict:
    idx: int
    category: str
    correct: bool
    recall_s: float
    answer_s: float
    tokens: int
    cost: float
    predicted: str = ""


async def run_qa(sample_id: str, config: str, idx: int, qa: dict, kb: Path, state: Path,
                 conv_full: str, sem: asyncio.Semaphore) -> Verdict:
    cat = qa.get("category", 0)
    catname = CATEGORY.get(cat, str(cat))
    cdir = CACHE / "qa" / _RUN_TAG / sample_id / config
    cdir.mkdir(parents=True, exist_ok=True)
    cf = cdir / f"{idx:04d}.json"
    if cf.exists():
        d = json.loads(cf.read_text())
        return Verdict(idx, catname, d["correct"], d["recall_s"], d["answer_s"],
                       d["tokens"], d["cost"], d.get("predicted", ""))

    question = qa["question"]
    gold = gold_of(qa)
    recall_s = 0.0
    if config == "arms_on":
        context, recall_s = await recall_context(question, kb, state, True)
    elif config == "arms_off":
        context, recall_s = await recall_context(question, kb, state, False)
    elif config == "full_context":
        context = conv_full
    else:  # no_memory
        context = ""

    t0 = time.perf_counter()
    ans = await claude(answer_prompt(question, context), ANSWER_SYS, sem)
    answer_s = time.perf_counter() - t0
    jud = await claude(judge_prompt(question, gold, ans.text, cat), JUDGE_SYS, sem)
    correct = False
    m = re.search(r"\{.*\}", jud.text, re.S)
    if m:
        try:
            correct = bool(json.loads(m.group(0)).get("correct"))
        except json.JSONDecodeError:
            pass
    tokens = ans.in_tok + ans.out_tok + ans.cache_tok + jud.in_tok + jud.out_tok + jud.cache_tok
    cost = ans.cost + jud.cost
    rec = {"correct": correct, "recall_s": recall_s, "answer_s": answer_s,
           "tokens": tokens, "cost": cost, "predicted": ans.text, "gold": gold,
           "category": catname, "question": question}
    cf.write_text(json.dumps(rec))
    return Verdict(idx, catname, correct, recall_s, answer_s, tokens, cost, ans.text)


# ----------------------------- score -------------------------------------

def pctl(xs: list[float], p: float) -> float | None:
    if not xs:
        return None
    s = sorted(xs)
    i = min(len(s) - 1, max(0, int(round(p / 100 * (len(s) + 1))) - 1))
    return round(s[i], 3)


def score(verdicts: list[Verdict], n_notes: int) -> dict:
    by_cat: dict[str, list[Verdict]] = {}
    for v in verdicts:
        by_cat.setdefault(v.category, []).append(v)
    per_cat = {c: {"n": len(vs), "j_score": round(sum(x.correct for x in vs) / len(vs), 4)}
               for c, vs in sorted(by_cat.items())}
    rec_lat = [v.recall_s for v in verdicts if v.recall_s > 0]
    ans_lat = [v.answer_s for v in verdicts]
    return {
        "n_qa": len(verdicts),
        "j_score": round(sum(v.correct for v in verdicts) / len(verdicts), 4) if verdicts else 0,
        "per_category": per_cat,
        "recall_latency_p50_s": pctl(rec_lat, 50),
        "recall_latency_p95_s": pctl(rec_lat, 95),
        "answer_latency_p50_s": pctl(ans_lat, 50),
        "answer_latency_p95_s": pctl(ans_lat, 95),
        "total_tokens": sum(v.tokens for v in verdicts),
        "total_cost_usd": round(sum(v.cost for v in verdicts), 4),
        "memory_notes": n_notes,
    }


# ----------------------------- driver ------------------------------------

async def ingest_sample(sample: dict, sem: asyncio.Semaphore) -> tuple[Path, Path, int]:
    sid = sample["sample_id"]
    kb = KB_ROOT / sid
    state = KB_ROOT / sid / ".state"
    conv = sample["conversation"]
    sess = sessions_sorted(conv)
    extracted = await asyncio.gather(*[
        extract_session(sid, skey, dt, turns, sem) for skey, dt, turns in sess])
    all_facts = [f for fs in extracted for f in fs]
    if kb.exists():
        shutil.rmtree(kb)
    n = write_notes(kb, all_facts)
    print(f"  [{sid}] extracted {n} notes from {len(sess)} sessions; reindexing…", flush=True)
    reindex(kb, state)
    return kb, state, n


def select_qa(sample: dict, limit_qa: int | None, per_cat: int | None) -> list[tuple[int, dict]]:
    """Return [(original_index, qa)]. per_cat takes the first N of EACH category
    (balanced strata); limit_qa takes the first N overall; else all. Original
    index is preserved so the per-QA cache is reused by a later full run."""
    pairs = list(enumerate(sample["qa"]))
    if per_cat:
        out: list[tuple[int, dict]] = []
        seen: dict[int, int] = {}
        for i, qa in pairs:
            c = qa.get("category", 0)
            if seen.get(c, 0) < per_cat:
                out.append((i, qa))
                seen[c] = seen.get(c, 0) + 1
        return out
    if limit_qa:
        return pairs[:limit_qa]
    return pairs


async def run_config(sample: dict, config: str, kb: Path, state: Path, n_notes: int,
                     conv_full: str, sem: asyncio.Semaphore, selected: list[tuple[int, dict]]) -> dict:
    sid = sample["sample_id"]
    verdicts = await asyncio.gather(*[
        run_qa(sid, config, i, qa, kb, state, conv_full, sem) for i, qa in selected])
    s = score(list(verdicts), n_notes)
    print(f"  [{sid}/{config}] J={s['j_score']:.3f}  n={s['n_qa']}  ${s['total_cost_usd']}", flush=True)
    return s


async def main_async(args) -> None:
    data = json.loads(DATA.read_text())
    if args.samples == "all":
        samples = data
    else:
        idxs = [int(x) for x in args.samples.split(",")]
        samples = [data[i] for i in idxs]
    configs = args.configs.split(",")
    sem = asyncio.Semaphore(args.concurrency)
    global _RECALL_SEM, RECALL_LIMIT, RECALL_MAX_CHARS, _RUN_TAG
    _RECALL_SEM = asyncio.Semaphore(args.recall_concurrency)
    RECALL_LIMIT = args.recall_limit
    RECALL_MAX_CHARS = args.recall_max_chars
    _RUN_TAG = args.tag
    RESULTS.mkdir(parents=True, exist_ok=True)
    print(f"recall: limit={RECALL_LIMIT} max_chars={RECALL_MAX_CHARS}", flush=True)

    report: dict = {"model": MODEL, "samples": [], "configs": configs}
    for sample in samples:
        sid = sample["sample_id"]
        print(f"== sample {sid}: {len(sample['qa'])} QA ==", flush=True)
        kb, state, n_notes = await ingest_sample(sample, sem)
        conv_full = full_conversation(sample["conversation"]) if "full_context" in configs else ""
        selected = select_qa(sample, args.limit_qa, args.per_cat)
        srow: dict = {"sample_id": sid, "n_notes": n_notes, "n_selected": len(selected), "by_config": {}}
        for config in configs:
            srow["by_config"][config] = await run_config(
                sample, config, kb, state, n_notes, conv_full, sem, selected)
        report["samples"].append(srow)

    out = RESULTS / f"report_{args.tag}.json"
    out.write_text(json.dumps(report, indent=2))
    print(f"\nwrote {out}", flush=True)
    print_summary(report)


def print_summary(report: dict) -> None:
    print("\n================ LOCOMO SUMMARY ================")
    cfgs = report["configs"]
    cats = ["single_hop", "multi_hop", "temporal", "open_domain", "adversarial"]
    # micro-average across samples: pool correct/n per config (+ per category)
    def micro(cfg: str, cat: str | None = None) -> tuple[int, int]:
        tot_n = tot_c = 0
        for s in report["samples"]:
            sc = s["by_config"][cfg]
            if cat is None:
                tot_n += sc["n_qa"]; tot_c += round(sc["j_score"] * sc["n_qa"])
            else:
                pc = sc["per_category"].get(cat)
                if pc:
                    tot_n += pc["n"]; tot_c += round(pc["j_score"] * pc["n"])
        return tot_c, tot_n

    hdr = f"{'config':<13}" + "".join(f"{c[:9]:>11}" for c in cats) + f"{'OVERALL':>11}"
    print(hdr); print("-" * len(hdr))
    for cfg in cfgs:
        row = f"{cfg:<13}"
        for cat in cats:
            c, n = micro(cfg, cat)
            row += f"{(c/n if n else 0):>10.3f}·" if n else f"{'—':>11}"
        c, n = micro(cfg)
        row += f"{(c/n if n else 0):>11.3f}"
        print(row)
    if "arms_on" in cfgs and "arms_off" in cfgs:
        on = micro("arms_on"); off = micro("arms_off")
        d = (on[0]/on[1] if on[1] else 0) - (off[0]/off[1] if off[1] else 0)
        print(f"\narms ablation Δ (on−off) overall: {d:+.3f}   "
              f"(arms_on {on[0]}/{on[1]}, arms_off {off[0]}/{off[1]})")
    tot_cost = sum(s["by_config"][c]["total_cost_usd"] for s in report["samples"] for c in cfgs)
    print(f"total cost: ${tot_cost:.2f}")


def parse_args():
    ap = argparse.ArgumentParser()
    ap.add_argument("--samples", default="0", help="comma idx (0..9) or 'all'")
    ap.add_argument("--configs", default="arms_on,arms_off,no_memory,full_context")
    ap.add_argument("--limit-qa", type=int, default=None, help="first-N QA per sample (smoke)")
    ap.add_argument("--per-cat", type=int, default=None, help="first-N QA of EACH category (balanced strata)")
    ap.add_argument("--concurrency", type=int, default=CONCURRENCY, help="claude answer/judge concurrency")
    ap.add_argument("--recall-concurrency", type=int, default=3, help="parallel recall.py procs (torch RAM-bound)")
    ap.add_argument("--recall-limit", type=int, default=RECALL_LIMIT, help="top-K notes to retrieve")
    ap.add_argument("--recall-max-chars", type=int, default=RECALL_MAX_CHARS, help="max chars of memory injected")
    ap.add_argument("--tag", default="pilot")
    return ap.parse_args()


if __name__ == "__main__":
    if not DATA.exists():
        raise SystemExit(f"dataset missing: {DATA}")
    if not RECALL_PY.exists():
        raise SystemExit(f"recall.py missing: {RECALL_PY}")
    asyncio.run(main_async(parse_args()))
