"""Tests for the cascade pre-processing (W4): reflect_cascade.prepare + slicing.

The cascade's value is making /reflect's INPUT tiny and skipping worthless
runs for $0. prepare() is deterministic (no LLM) so it's fully testable here;
the actual Sonnet /reflect call is the drainer's job (integration-tested via
the drain script's dry-run cascade path).
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
SCRIPTS = PLUGIN_ROOT / "scripts"
DRAIN = PLUGIN_ROOT / "hooks" / "reflect-drain-bg.sh"
sys.path.insert(0, str(SCRIPTS))

import reflect_cascade  # noqa: E402


def _write(path: Path, turns: list[tuple[str, str]]) -> Path:
    with open(path, "w") as fh:
        for role, text in turns:
            fh.write(json.dumps({"message": {"role": role, "content": text}}) + "\n")
    return path


def _reflect_on_reflect(path: Path) -> Path:
    path.write_text(json.dumps({"message": {
        "role": "user",
        "content": "<command-name>reflect</command-name>\nProcess the transcript at: /x.jsonl",
    }}) + "\n")
    return path


# ── prepare: gate verdicts ───────────────────────────────────────────────────

def test_prepare_skips_reflect_on_reflect(tmp_path):
    prep = reflect_cascade.prepare(_reflect_on_reflect(tmp_path / "ror.jsonl"))
    assert prep.action == "skip" and prep.reason == "reflect-on-reflect"
    assert prep.slice_path is None


def test_prepare_skips_clean_session(tmp_path):
    t = _write(tmp_path / "clean.jsonl", [
        ("user", "Morning. Summarize the attached document for me."),
        ("assistant", "It covers three topics: weather, travel, cooking."),
    ])
    prep = reflect_cascade.prepare(t)
    assert prep.action == "skip" and prep.reason == "no-signal"


def test_prepare_reflects_and_slices(tmp_path):
    # Many filler lines + one real correction far down: the slice must be much
    # smaller than the original dialogue.
    filler = [("assistant", f"Step {i}: routine progress note number {i}.") for i in range(400)]
    turns = filler + [("user", "No, never use var here. The root cause was a missing index.")] + filler
    t = _write(tmp_path / "big.jsonl", turns)
    prep = reflect_cascade.prepare(t, out_path=str(tmp_path / "slice.txt"))
    assert prep.action == "reflect"
    assert prep.signal_count > 0
    assert prep.slice_path and Path(prep.slice_path).exists()
    # The whole point: the slice is a fraction of the original.
    assert prep.slice_tokens < prep.orig_tokens
    body = Path(prep.slice_path).read_text()
    assert "never use var" in body  # the signal survived into the slice


def test_prepare_dedups_by_signal_hash(tmp_path, monkeypatch):
    t = _write(tmp_path / "sig.jsonl", [
        ("user", "No, never use var. The root cause was a missing index."),
    ])
    monkeypatch.setattr(reflect_cascade, "_signal_hash_seen", lambda h: True)
    prep = reflect_cascade.prepare(t)
    assert prep.action == "skip" and prep.reason == "dup-signal-hash"


# ── slice_dialogue ───────────────────────────────────────────────────────────

class _Sig:
    def __init__(self, line_number, signal="x"):
        self.line_number = line_number
        self.signal = signal


def test_slice_keeps_windows_and_drops_filler():
    lines = [f"line{i}" for i in range(100)]
    text = "\n".join(lines)
    # line_number is 1-based (signal_detector); two windows with filler between.
    sliced = reflect_cascade.slice_dialogue(text, [_Sig(21), _Sig(81)], context_lines=2)
    assert "line20" in sliced and "line80" in sliced  # window centres kept
    assert "line50" not in sliced                      # filler between dropped
    assert "…" in sliced                               # gap marker between windows


def test_slice_respects_max_chars():
    lines = [f"line{i}" for i in range(1000)]
    text = "\n".join(lines)
    sigs = [_Sig(i) for i in range(0, 1000, 2)]  # signals everywhere
    sliced = reflect_cascade.slice_dialogue(text, sigs, context_lines=1, max_chars=200)
    assert len(sliced) <= 260  # cap + truncation marker slack


# ── drainer integration: cascade skip is $0 ─────────────────────────────────

def test_drainer_cascade_skip_is_free(tmp_path):
    state = tmp_path / "state"
    state.mkdir()
    ror = state / "ror.jsonl"
    _reflect_on_reflect(ror)
    (state / "pending_reflections.jsonl").write_text(
        json.dumps({"ts": "t", "session_id": "s", "transcript_path": str(ror),
                    "trigger": "stop", "cwd": "/"}) + "\n"
    )
    import os
    env = dict(os.environ)
    env.update({
        "REFLECT_STATE_DIR": str(state),
        "REFLECT_DRAIN_DRY_RUN": "1",
        "REFLECT_DRAIN_SKIP_REINDEX": "1",
        "REFLECT_DRAIN_DEBOUNCE_SEC": "0",
    })
    subprocess.run(["bash", str(DRAIN)], env=env, capture_output=True, text=True, timeout=60)
    cost = (state / "drain-cost.jsonl").read_text()
    rec = json.loads([l for l in cost.splitlines() if l.strip()][0])
    assert rec["outcome"] == "skip_reflect_on_reflect"
    assert rec["entries"] == 0  # no model call, no cap consumed
    # And it was removed from the queue.
    assert (state / "pending_reflections.jsonl").read_text().strip() == ""


# ── S7 delta retain / chunk-hash dedup ───────────────────────────────────────

import importlib  # noqa: E402


@pytest.fixture(autouse=True)
def _isolate_reflect_db(tmp_path, monkeypatch):
    """Route reflect_db at a fresh per-test tmp DB.

    S7 gave prepare() a write side effect (recording chunk hashes), so EVERY
    cascade test must isolate the DB or it would mutate — and read stale state
    from — the developer's real ~/.reflect/reflect.db. Autouse keeps that
    guarantee blanket across the module.
    """
    import reflect_db
    monkeypatch.setenv("REFLECT_DB_PATH", str(tmp_path / "reflect.db"))
    import reflect_config
    importlib.reload(reflect_config)
    reflect_db.close_all()
    yield
    reflect_db.close_all()


@pytest.fixture
def chunk_db():
    """The isolated reflect_db module (DB routing is handled by the autouse
    fixture above). Returned so chunk-hash tests can call its helpers directly."""
    import reflect_db
    return reflect_db


def _signal_transcript(path: Path, marker: str) -> Path:
    """A transcript with filler + one correction window carrying *marker*."""
    turns = [("user", f"filler {i}") for i in range(30)]
    turns.append((
        "user",
        f"No, never use var in the {marker} module. The root cause was a "
        f"missing index on {marker}. Use let instead.",
    ))
    turns += [("assistant", f"filler {i}") for i in range(30)]
    return _write(path, turns)


def test_split_slice_chunks_recovers_windows():
    sliced = "alpha line\n…\nbeta line\n… [slice truncated]"
    chunks = reflect_cascade.split_slice_chunks(sliced)
    assert chunks == ["alpha line", "beta line"]  # gap + truncation markers dropped


def test_rerun_identical_transcript_skips(chunk_db, tmp_path):
    """Acceptance: re-running drain on an identical transcript yields 0 new
    chunks → the cascade skips it → 0 new learnings."""
    t = _signal_transcript(tmp_path / "id.jsonl", "auth")
    p1 = reflect_cascade.prepare(t, out_path=str(tmp_path / "s1.txt"))
    assert p1.action == "reflect"
    assert p1.chunks_total >= 1 and p1.chunks_skipped == 0

    p2 = reflect_cascade.prepare(t, out_path=str(tmp_path / "s2.txt"))
    assert p2.action == "skip" and p2.reason == "dup-chunk-hash"
    assert p2.chunks_skipped == p2.chunks_total == p1.chunks_total


def test_grown_transcript_reflects_only_new_chunk(chunk_db, tmp_path):
    """A transcript that grows by one new exchange re-reflects ONLY the new
    chunk; the already-reflected window is deduped out of the slice."""
    base = [("user", f"filler {i}") for i in range(25)]
    base.append((
        "user",
        "No, never use var in the cache module. The root cause was a missing index.",
    ))
    base += [("assistant", f"filler {i}") for i in range(25)]
    t1 = _write(tmp_path / "base.jsonl", base)
    p1 = reflect_cascade.prepare(t1, out_path=str(tmp_path / "b1.txt"))
    assert p1.action == "reflect" and p1.chunks_skipped == 0

    grown = list(base)
    grown += [("user", f"more {i}") for i in range(25)]
    grown.append((
        "user",
        "Actually, prefer async here. The bug was a race condition in the queue.",
    ))
    grown += [("assistant", f"more {i}") for i in range(25)]
    t2 = _write(tmp_path / "grown.jsonl", grown)
    p2 = reflect_cascade.prepare(t2, out_path=str(tmp_path / "g2.txt"))

    assert p2.action == "reflect"
    assert p2.chunks_total == 2 and p2.chunks_skipped == 1
    slice_text = Path(p2.slice_path).read_text()
    assert "race condition" in slice_text          # the NEW chunk reflects
    assert "missing index" not in slice_text         # the OLD chunk is deduped out


def test_chunk_hash_ttl_lets_chunk_reflect_again(chunk_db, tmp_path):
    """Acceptance: the hash table is TTL'd — a chunk recorded longer than the
    retention window is pruned, so an identical transcript reflects again
    instead of being wedged out of re-reflection forever."""
    from datetime import datetime, timedelta, timezone

    t = _signal_transcript(tmp_path / "ttl.jsonl", "billing")
    p1 = reflect_cascade.prepare(t, out_path=str(tmp_path / "t1.txt"))
    assert p1.action == "reflect"

    # Age every recorded chunk hash past the TTL window, then prune.
    conn = chunk_db.get_conn()
    stale = (datetime.now(timezone.utc)
             - timedelta(days=chunk_db.CHUNK_HASH_TTL_DAYS + 1)).isoformat()
    with conn:
        conn.execute("UPDATE chunk_hashes SET created_at = ?", (stale,))
    assert chunk_db.get_seen_chunk_hashes(conn=conn)  # rows present pre-prune

    p2 = reflect_cascade.prepare(t, out_path=str(tmp_path / "t2.txt"))
    # prepare() prunes the stale row internally → the chunk is fresh again.
    assert p2.action == "reflect" and p2.chunks_skipped == 0


def test_db_helpers_record_seen_and_prune(chunk_db):
    """Direct DB-level pins for the chunk-hash store: record is idempotent,
    membership reflects records, and prune respects the TTL boundary."""
    from datetime import datetime, timedelta, timezone

    conn = chunk_db.get_conn()
    h = chunk_db.compute_chunk_hash("use ripgrep not grep")
    assert chunk_db.compute_chunk_hash("use ripgrep not grep   ") == h  # ws-stable

    assert chunk_db.get_seen_chunk_hashes([h], conn=conn) == set()
    assert chunk_db.record_chunk_hashes([h], conn=conn) == 1
    assert chunk_db.record_chunk_hashes([h], conn=conn) == 0  # idempotent
    assert chunk_db.get_seen_chunk_hashes([h], conn=conn) == {h}

    old = (datetime.now(timezone.utc) - timedelta(days=999)).isoformat()
    old_h = chunk_db.compute_chunk_hash("ancient chunk")
    chunk_db.record_chunk_hashes([old_h], now=old, conn=conn)
    assert chunk_db.prune_chunk_hashes(conn=conn) == 1     # only the aged row
    assert chunk_db.get_seen_chunk_hashes(conn=conn) == {h}  # recent survives
    assert chunk_db.prune_chunk_hashes(ttl_days=0, conn=conn) == 0  # disabled


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
