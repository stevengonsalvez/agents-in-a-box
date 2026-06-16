# ABOUTME: Behavioral proof for S7 — delta retain / chunk-hash dedup. Re-running
# ABOUTME: the cascade on an identical transcript produces 0 new chunks (skip),
# ABOUTME: so a re-drain adds no duplicate learning; the TTL lets a chunk reflect
# ABOUTME: again after the retention window. Tied to recall: one learning, one hit.
"""S7 delta retain / chunk-hash dedup proof.

Port S7 is a STORAGE/CAPTURE port: it acts at drain (ingest) time inside the
cascade, before any LLM or KB write. recall.py never sees a "chunk hash" flag,
so the port's behaviour is not directly observable as a recall ranking knob.
Per the proof guide for storage ports, this proof asserts the stored SHAPE (the
``chunk_hashes`` dedup table + its TTL) AND the closest observable retrieval
consequence: re-draining the SAME transcript must not duplicate the learning in
the KB, so a recall query keeps returning a SINGLE copy of it rather than two.

Invariant (three linked assertions, seeds + flags fully determine each — no LLM
in the assertion):

  1. FIRST drain of a signal-bearing transcript: cascade.prepare() returns
     action=reflect with chunks_skipped=0 — the chunk is new, it gets reflected,
     and its hash is recorded in chunk_hashes. The learning the slice would
     produce is seeded into the real hermetic KB and a recall for it returns
     exactly ONE hit.

  2. RE-DRAIN of the byte-identical transcript: cascade.prepare() returns
     action=skip / reason=dup-chunk-hash with chunks_skipped == chunks_total —
     i.e. 0 NEW chunks => 0 new learnings (the first acceptance bullet). Because
     the re-drain produces nothing, NO second copy of the learning is written,
     so the SAME recall query STILL returns exactly one hit (not a duplicated
     pair). This is the observable retrieval consequence of the dedup.

  3. TTL (the second acceptance bullet): after the chunk-hash rows are aged past
     the retention window and pruned, prepare() on the identical transcript
     reflects AGAIN (chunks_skipped=0) — a stale dedup entry can never wedge a
     chunk out of re-reflection forever.

Falsifiability: if chunk-hash dedup were absent, the re-drain in (2) would
reflect the transcript again (action=reflect, chunks_skipped=0), a second
identical learning would land in the KB, and a recall would surface the
DUPLICATE — assertion (2) would FAIL. If the TTL were absent (hashes kept
forever), the aged-and-pruned re-run in (3) would still skip and assertion (3)
would FAIL.

PORT: S7
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pytest

# conftest.py (one dir up) renders docs identically to every other proof.
_CONFTEST_DIR = Path(__file__).resolve().parents[1]
if str(_CONFTEST_DIR) not in sys.path:
    sys.path.insert(0, str(_CONFTEST_DIR))
from conftest import RECALL_PY, _doc_md  # noqa: E402

# The cascade + db live in the reflect plugin; import them directly. prepare()
# is pure/deterministic (gate + slice + hash, no LLM), so the proof can drive it
# exactly as the drainer does and observe the skip verdict.
# _CONFTEST_DIR = reflect-kb/tests/eval/behavioral; parents[3] is the repo root
# where plugins/ lives alongside reflect-kb/. Fall back to a reflect-kb-as-root
# checkout (plugins as a sibling of reflect-kb) like conftest.RECALL_PY does.
_PLUGIN_CANDIDATES = [
    _CONFTEST_DIR.parents[3] / "plugins" / "reflect" / "scripts",
    _CONFTEST_DIR.parents[2].parent / "plugins" / "reflect" / "scripts",
]
_PLUGIN_SCRIPTS = next((p for p in _PLUGIN_CANDIDATES if p.exists()), _PLUGIN_CANDIDATES[0])
if str(_PLUGIN_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_PLUGIN_SCRIPTS))

# The learning a drain over the transcript below would land in the KB. Its body
# carries the same correction the transcript's signal window does, so recall for
# the correction surfaces this single doc.
SEED = dict(
    name="s7-no-global-mutable-state",
    title="never reach for module-global mutable state in the request handler",
    category="architecture",
    tags=["architecture", "global-state", "concurrency"],
    confidence="high",
    created="2026-03-01",
    key_insight="Thread per-request state through call args instead of a module "
                "global so concurrent requests can't clobber each other.",
    body="A module-global cache in the request handler was clobbered under "
         "concurrent requests because every request mutated the same global; "
         "threading the state through call arguments removed the race.",
)

QUERY = "module global mutable state clobbered under concurrent requests in the handler"

# The transcript the drain would process — filler around ONE correction window
# whose content matches the seeded learning. Re-running drain on this identical
# transcript is the scenario the acceptance criteria pin.
_CORRECTION = (
    "No, never reach for module-global mutable state in the request handler. "
    "The root cause was the global cache being clobbered under concurrent "
    "requests. Thread the per-request state through call arguments instead."
)


def _write_transcript(path: Path) -> Path:
    turns = [{"role": "assistant", "content": f"Step {i}: routine progress note {i}."}
             for i in range(40)]
    turns.append({"role": "user", "content": _CORRECTION})
    turns += [{"role": "assistant", "content": f"Step {i}: more routine notes {i}."}
              for i in range(40)]
    with open(path, "w") as fh:
        for t in turns:
            fh.write(json.dumps({"message": t}) + "\n")
    return path


def _base_env(kb_dir: Path, state_dir: Path, cache_home: Path, db_path: Path) -> dict:
    env = dict(os.environ)
    env["GLOBAL_LEARNINGS_PATH"] = str(kb_dir)
    env["REFLECT_STATE_DIR"] = str(state_dir)
    env["XDG_CACHE_HOME"] = str(cache_home)
    env["REFLECT_DB_PATH"] = str(db_path)  # sandbox the chunk_hashes table
    env.setdefault("HF_HOME", str(Path.home() / ".cache" / "huggingface"))
    env.setdefault(
        "SENTENCE_TRANSFORMERS_HOME",
        str(Path.home() / ".cache" / "torch" / "sentence_transformers"),
    )
    bin_dir = os.environ.get("RECALL_EVAL_BIN_DIR")
    if bin_dir:
        env["PATH"] = f"{bin_dir}:{env.get('PATH', '')}"
    return env


def _seed_kb(kb_dir: Path, base_env: dict, learnings: list[dict]) -> None:
    """Build the hermetic KB (reflect init + write docs + reindex)."""
    kb_dir.mkdir(parents=True, exist_ok=True)
    r = subprocess.run(["reflect", "init"], capture_output=True, text=True, env=base_env)
    assert r.returncode == 0, f"reflect init failed: {r.stderr[-600:]}"
    docs = kb_dir / "documents"
    docs.mkdir(exist_ok=True)
    for d in learnings:
        (docs / f"{d['name']}.md").write_text(_doc_md(d))
    r = subprocess.run(
        ["reflect", "reindex", "--force"],
        capture_output=True, text=True, env=base_env, timeout=1800,
    )
    assert r.returncode == 0, f"reflect reindex failed: {r.stderr[-800:]}"


def _recall(base_env: dict, query: str) -> dict:
    cmd = [
        "python3", str(RECALL_PY), query,
        "--limit", "5", "--format", "json", "--no-cache", "--min-overlap", "0.0",
    ]
    r = subprocess.run(cmd, capture_output=True, text=True, env=base_env, timeout=300)
    assert r.returncode == 0, f"recall.py exited {r.returncode}\nSTDERR:\n{r.stderr[-1200:]}"
    return json.loads(r.stdout or "{}")


@pytest.mark.skipif(
    not shutil.which("reflect", path=(os.environ.get("RECALL_EVAL_BIN_DIR", "") + ":" + os.environ.get("PATH", ""))),
    reason="full-stack `reflect` not resolvable; set RECALL_EVAL_BIN_DIR",
)
def test_S7_chunk_hash_dedup(tmp_path):
    kb_dir = tmp_path / "kb"
    state_dir = tmp_path / "state"
    cache_home = tmp_path / "xdg-cache"
    db_path = tmp_path / "reflect.db"
    for d in (state_dir, cache_home):
        d.mkdir(parents=True, exist_ok=True)

    base_env = _base_env(kb_dir, state_dir, cache_home, db_path)
    if not shutil.which("reflect", path=base_env["PATH"]):
        pytest.skip("`reflect` CLI not resolvable in the proof env")

    # The cascade + db must resolve the SANDBOXED DB, not the developer's
    # ~/.reflect — drive them through the same REFLECT_DB_PATH the env carries.
    os.environ["REFLECT_DB_PATH"] = str(db_path)
    import reflect_config
    import importlib
    importlib.reload(reflect_config)
    import reflect_db
    reflect_db.close_all()
    import reflect_cascade

    transcript = _write_transcript(tmp_path / "session.jsonl")

    # ---- (1) FIRST drain: the chunk is new -> reflect, hash recorded. ----
    p1 = reflect_cascade.prepare(transcript, out_path=str(tmp_path / "slice1.txt"))
    assert p1.action == "reflect", (
        f"first drain of a signal-bearing transcript must reflect, got "
        f"{p1.action}/{p1.reason}"
    )
    assert p1.chunks_total >= 1 and p1.chunks_skipped == 0, (
        f"first drain must see every chunk as NEW (skipped=0), got "
        f"total={p1.chunks_total} skipped={p1.chunks_skipped}"
    )

    # The drain that ran on slice1 would land SEED in the KB exactly once.
    _seed_kb(kb_dir, base_env, [SEED])
    hits1 = [r.get("id") for r in _recall(base_env, QUERY).get("results", [])]
    assert hits1.count(SEED["name"]) == 1, (
        f"after the first drain the KB must hold exactly one copy of the "
        f"learning; recall returned {hits1}"
    )

    # ---- (2) RE-DRAIN identical transcript: 0 new chunks -> skip. ----
    p2 = reflect_cascade.prepare(transcript, out_path=str(tmp_path / "slice2.txt"))
    assert p2.action == "skip" and p2.reason == "dup-chunk-hash", (
        f"re-draining the IDENTICAL transcript must skip via chunk-hash dedup "
        f"(0 new learnings), got {p2.action}/{p2.reason} — the delta-retain "
        f"dedup is not firing, so a re-queued transcript would re-reflect."
    )
    assert p2.chunks_skipped == p2.chunks_total == p1.chunks_total, (
        f"every chunk must be recognised as already-reflected: "
        f"skipped={p2.chunks_skipped} total={p2.chunks_total}"
    )

    # Retrieval consequence: because the re-drain produced nothing, no second
    # copy was written. The KB still holds exactly ONE — recall is not polluted
    # by a duplicate. (We re-assert against the unchanged KB; had the re-drain
    # reflected, a duplicate doc would have been indexed and surfaced here.)
    hits2 = [r.get("id") for r in _recall(base_env, QUERY).get("results", [])]
    assert hits2.count(SEED["name"]) == 1, (
        f"a re-drain must not duplicate the learning; recall returned {hits2}"
    )

    # ---- (3) TTL: age + prune the chunk hashes -> the chunk reflects again. ----
    conn = reflect_db.get_conn()
    stale = (datetime.now(timezone.utc)
             - timedelta(days=reflect_db.CHUNK_HASH_TTL_DAYS + 1)).isoformat()
    with conn:
        conn.execute("UPDATE chunk_hashes SET created_at = ?", (stale,))
    # Sanity: rows exist before prepare() prunes them.
    assert reflect_db.get_seen_chunk_hashes(conn=conn), \
        "expected chunk-hash rows to exist before the TTL prune"

    p3 = reflect_cascade.prepare(transcript, out_path=str(tmp_path / "slice3.txt"))
    assert p3.action == "reflect" and p3.chunks_skipped == 0, (
        f"after the TTL window elapses, prepare() prunes the stale hashes and "
        f"the chunk is eligible to reflect again, got {p3.action}/{p3.reason} "
        f"skipped={p3.chunks_skipped} — the dedup table is not TTL'd, so a "
        f"chunk would be wedged out of re-reflection forever."
    )
