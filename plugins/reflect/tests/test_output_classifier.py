# ABOUTME: Regression tests for port M2 — writer-output classifier + respawn circuit breaker.
# ABOUTME: Pins every M2 acceptance bullet: 5-way category, 3-strike respawn, category logging, valid-reset, env threshold.
"""Port M2 (pattern: claude-mem output-classifier + INVALID_OUTPUT_RESPAWN_THRESHOLD):
every drain writer output is classified into {valid, prose, idle, poisoned,
malformed}; three consecutive non-valid outputs (or one poisoned wedge) trip
the writer_drift breaker — kill + archive the entry, with the offending
categories logged — and a valid output resets the streak.

Unit tests exercise classify()/track() directly; integration tests shell out
to reflect-drain-bg.sh with an isolated REFLECT_STATE_DIR and a stub claude
binary so nothing touches the real ~/.reflect or spends tokens.
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
SCRIPTS = PLUGIN_ROOT / "scripts"
DRAIN = PLUGIN_ROOT / "hooks" / "reflect-drain-bg.sh"
CLASSIFIER = SCRIPTS / "output_classifier.py"

sys.path.insert(0, str(SCRIPTS))

from output_classifier import CATEGORIES, classify, default_threshold, track  # noqa: E402


VALID_ENVELOPE = json.dumps({
    "type": "result", "subtype": "success", "is_error": False,
    "result": "Captured 1 learning about pytest fixtures.",
    "total_cost_usd": 0.012, "num_turns": 3,
    "usage": {"input_tokens": 100, "output_tokens": 50,
              "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0},
})


# ---------- acceptance 1: exactly one of the 5 categories, for any string ----------

@pytest.mark.parametrize("raw,expected", [
    (VALID_ENVELOPE, "valid"),
    ('{"is_error": false, "result": ""}', "valid"),
    ("I'm sorry, I can't extract learnings from that transcript.", "prose"),
    ("Sure! Here is a summary of what happened in the session...", "prose"),
    ("", "idle"),
    ("   \n\t  ", "idle"),
    ("Error: Prompt is too long: 250000 tokens > 200000 maximum", "poisoned"),
    ("the conversation is too long to continue", "poisoned"),
    ('{"is_error": true, "result": "Prompt is too long"}', "poisoned"),
    ('{"type":"result","is_error":false,"result":"Captur', "malformed"),  # truncated
    ("[1, 2, 3]", "malformed"),          # JSON, wrong shape
    ('"just a json string"', "malformed"),
    ('{"foo": "bar"}', "malformed"),     # dict but not a result envelope
])
def test_classify_known_shapes(raw, expected):
    got = classify(raw)
    assert got == expected
    assert got in CATEGORIES


@pytest.mark.parametrize("raw", [
    None, 42, b"bytes", ["list"],                       # non-strings -> idle
    "x", "{", "[", "\x00\x01garbage\xff", "}" * 1000,
    "<observation>not our schema</observation>",
    json.dumps({"deeply": {"nested": [1, {"weird": None}]}}),
    "a" * 100_000,
])
def test_classify_total_over_arbitrary_input(raw):
    """Classifier never raises and always returns exactly one known category."""
    assert classify(raw) in CATEGORIES


def test_healthy_envelope_mentioning_marker_stays_valid():
    """A successful learning that merely *mentions* context windows must not
    be misclassified as a poisoned writer (false-positive guard)."""
    raw = json.dumps({"type": "result", "is_error": False,
                      "result": "Learned: keep prompts under the context window."})
    assert classify(raw) == "valid"


# ---------- acceptance 2 + 3 (unit): 3 consecutive invalids => respawn, categories kept ----------

def test_three_consecutive_invalids_trigger_respawn(tmp_path):
    state = tmp_path / "writer-health.jsonl"
    t = "/tmp/t1.jsonl"
    h1 = track(state, t, "prose", threshold=3)
    h2 = track(state, t, "idle", threshold=3)
    assert (h1.respawn, h2.respawn) == (False, False)
    assert (h1.consecutive, h2.consecutive) == (1, 2)
    h3 = track(state, t, "malformed", threshold=3)
    assert h3.respawn is True
    assert h3.consecutive == 3
    # acceptance 3: the verdict carries the categories of all three offenders.
    assert h3.categories == ["prose", "idle", "malformed"]


def test_poisoned_triggers_immediate_respawn(tmp_path):
    """A wedged-session marker is deterministic — one strike, not three."""
    h = track(tmp_path / "wh.jsonl", "/tmp/t.jsonl", "poisoned", threshold=3)
    assert h.respawn is True
    assert h.categories == ["poisoned"]


def test_streak_resets_after_respawn(tmp_path):
    state = tmp_path / "wh.jsonl"
    t = "/tmp/t.jsonl"
    for _ in range(3):
        h = track(state, t, "prose", threshold=3)
    assert h.respawn is True
    # A re-enqueued transcript starts from a clean slate.
    h = track(state, t, "prose", threshold=3)
    assert h.consecutive == 1 and h.respawn is False


def test_streaks_are_per_transcript(tmp_path):
    state = tmp_path / "wh.jsonl"
    track(state, "/tmp/a.jsonl", "prose", threshold=3)
    track(state, "/tmp/a.jsonl", "prose", threshold=3)
    hb = track(state, "/tmp/b.jsonl", "prose", threshold=3)
    assert hb.consecutive == 1  # b is not contaminated by a's streak


# ---------- acceptance 4 (unit): valid output resets the counter ----------

def test_valid_resets_counter(tmp_path):
    state = tmp_path / "wh.jsonl"
    t = "/tmp/t.jsonl"
    track(state, t, "prose", threshold=3)
    track(state, t, "prose", threshold=3)
    h = track(state, t, "valid", threshold=3)
    assert h.consecutive == 0 and h.categories == [] and h.respawn is False
    # The next invalid starts a fresh streak — no respawn until 3 NEW invalids.
    assert track(state, t, "prose", threshold=3).respawn is False
    assert track(state, t, "prose", threshold=3).respawn is False
    assert track(state, t, "prose", threshold=3).respawn is True


# ---------- acceptance 5: threshold configurable via env var (default 3) ----------

def test_default_threshold_is_three(monkeypatch):
    monkeypatch.delenv("REFLECT_DRAIN_INVALID_THRESHOLD", raising=False)
    assert default_threshold() == 3


def test_threshold_env_var_overrides(tmp_path, monkeypatch):
    monkeypatch.setenv("REFLECT_DRAIN_INVALID_THRESHOLD", "2")
    state = tmp_path / "wh.jsonl"
    t = "/tmp/t.jsonl"
    assert track(state, t, "prose").respawn is False   # threshold=None -> env
    assert track(state, t, "prose").respawn is True


def test_garbage_threshold_env_falls_back_to_default(monkeypatch):
    monkeypatch.setenv("REFLECT_DRAIN_INVALID_THRESHOLD", "banana")
    assert default_threshold() == 3
    monkeypatch.setenv("REFLECT_DRAIN_INVALID_THRESHOLD", "-1")
    assert default_threshold() == 3


# ---------- CLI surface (what the drain hook shells out to) ----------

def test_cli_classify_reads_stdin():
    cp = subprocess.run(
        [sys.executable, str(CLASSIFIER), "classify"],
        input=VALID_ENVELOPE, capture_output=True, text=True, timeout=30,
    )
    assert cp.returncode == 0
    assert cp.stdout.strip() == "valid"


def test_cli_track_emits_verdict_json(tmp_path):
    state = tmp_path / "wh.jsonl"
    out = None
    for _ in range(3):
        cp = subprocess.run(
            [sys.executable, str(CLASSIFIER), "track", "--state", str(state),
             "--transcript", "/tmp/t.jsonl", "--category", "prose", "--threshold", "3"],
            capture_output=True, text=True, timeout=30,
        )
        assert cp.returncode == 0
        out = json.loads(cp.stdout)
    assert out["respawn"] is True
    assert out["consecutive"] == 3
    assert out["categories"] == ["prose", "prose", "prose"]


# ---------- integration: the drain hook trips the breaker end-to-end ----------

def _seed_queue(state_dir: Path) -> Path:
    state_dir.mkdir(parents=True, exist_ok=True)
    transcript = state_dir / "fake-transcript.jsonl"
    transcript.write_text("{}\n")
    queue = state_dir / "pending_reflections.jsonl"
    queue.write_text(
        '{"ts":"t","session_id":"s1","transcript_path":"%s",'
        '"trigger":"stop","cwd":"/"}\n' % transcript
    )
    return transcript


def _make_stub_claude(tmp_path: Path, outputs: list[str]) -> Path:
    """Stub `claude` that emits outputs[n] on its n-th invocation (last repeats)."""
    counter = tmp_path / "stub-calls"
    counter.write_text("0")
    payloads = tmp_path / "stub-payloads"
    payloads.mkdir(exist_ok=True)
    for i, out in enumerate(outputs):
        (payloads / f"{i}.out").write_text(out)
    stub = tmp_path / "stub-claude"
    stub.write_text(
        "#!/usr/bin/env bash\n"
        f'C="{counter}"\nP="{payloads}"\n'
        'n=$(cat "$C")\n'
        'echo $((n + 1)) > "$C"\n'
        'last=$(ls "$P" | wc -l)\n'
        'idx=$n; if [ "$idx" -ge "$last" ]; then idx=$((last - 1)); fi\n'
        'cat "$P/$idx.out"\n'
    )
    stub.chmod(stub.stat().st_mode | stat.S_IEXEC)
    return stub


def _run_drain(state_dir: Path, stub: Path, **env_overrides) -> subprocess.CompletedProcess:
    env = dict(os.environ)
    env.update({
        "REFLECT_STATE_DIR": str(state_dir),
        "REFLECT_DRAIN_DRY_RUN": "0",            # the breaker sits AFTER the writer call
        "REFLECT_DRAIN_CLAUDE_BIN": str(stub),
        "REFLECT_DRAIN_SKIP_REINDEX": "1",
        "REFLECT_DRAIN_DEBOUNCE_SEC": "0",
        "REFLECT_DRAIN_CASCADE": "0",
        "REFLECT_DRAIN_MAX_RETRIES": "10",       # keep the legacy retry-poison out of the way
        "REFLECT_QUIET_INSTALL_WARNING": "1",
    })
    env.update({k: str(v) for k, v in env_overrides.items()})
    return subprocess.run(
        ["bash", str(DRAIN)], env=env, capture_output=True, text=True, timeout=60
    )


def _cost_events(state_dir: Path) -> list[dict]:
    f = state_dir / "drain-cost.jsonl"
    if not f.exists():
        return []
    return [json.loads(l) for l in f.read_text().splitlines() if l.strip()]


def test_drain_respawns_after_three_consecutive_prose_outputs(tmp_path):
    """Acceptance 2+3 end-to-end: three drain runs each getting prose from the
    writer -> the third trips the breaker, archives the entry as writer_drift
    poison, and logs the respawn with all three offending categories."""
    state = tmp_path / "state"
    _seed_queue(state)
    stub = _make_stub_claude(tmp_path, ["Sorry, I could not find any learnings here."])

    for _ in range(2):
        _run_drain(state, stub)
        # Still in the queue (retryable fail), not yet poisoned.
        assert (state / "pending_reflections.jsonl").read_text().strip()
        assert not (state / "poison-reflections.jsonl").exists()

    _run_drain(state, stub)

    log = (state / "drain.log").read_text()
    assert "WRITER RESPAWN (writer_drift)" in log
    assert "categories=[prose,prose,prose]" in log          # acceptance 3
    assert "3 consecutive invalid" in log
    # Entry archived (the poison path), queue drained.
    assert (state / "poison-reflections.jsonl").read_text().strip()
    assert (state / "pending_reflections.jsonl").read_text().strip() == ""
    # Classification recorded in the drain-cost envelope (writer-health view).
    events = _cost_events(state)
    drift = [e for e in events if e["outcome"] == "poison_writer_drift"]
    assert len(drift) == 1
    assert drift[0]["writer_class"] == "prose"
    assert all(e.get("writer_class") == "prose" for e in events)


def test_drain_threshold_env_var_respected(tmp_path):
    """Acceptance 5 end-to-end: REFLECT_DRAIN_INVALID_THRESHOLD=1 poisons on
    the very first invalid output."""
    state = tmp_path / "state"
    _seed_queue(state)
    stub = _make_stub_claude(tmp_path, ["just some prose, no envelope"])
    _run_drain(state, stub, REFLECT_DRAIN_INVALID_THRESHOLD="1")
    log = (state / "drain.log").read_text()
    assert "WRITER RESPAWN (writer_drift)" in log
    assert (state / "poison-reflections.jsonl").read_text().strip()


def test_drain_valid_output_resets_streak(tmp_path):
    """Acceptance 4 end-to-end: prose, prose, VALID, prose never respawns —
    the valid envelope in run 3 reset the streak."""
    state = tmp_path / "state"
    _seed_queue(state)
    stub = _make_stub_claude(
        tmp_path,
        ["prose run one", "prose run two", VALID_ENVELOPE, "prose run four"],
    )
    _run_drain(state, stub)
    _run_drain(state, stub)
    _run_drain(state, stub)   # valid -> entry drained OK, streak reset
    assert (state / "pending_reflections.jsonl").read_text().strip() == ""
    _seed_queue(state)        # same transcript path comes back
    _run_drain(state, stub)   # prose again -> streak restarts at 1
    log = (state / "drain.log").read_text()
    assert "WRITER RESPAWN" not in log
    assert not (state / "poison-reflections.jsonl").exists()
    events = _cost_events(state)
    ok = [e for e in events if e["outcome"] == "ok"]
    assert len(ok) == 1 and ok[0]["writer_class"] == "valid"


def test_drain_poisoned_output_respawns_immediately(tmp_path):
    """A wedged writer ('prompt is too long') is killed on first sighting —
    no retry burn for a deterministic failure."""
    state = tmp_path / "state"
    _seed_queue(state)
    stub = _make_stub_claude(
        tmp_path, ['{"is_error": true, "result": "Prompt is too long: way over"}'],
    )
    _run_drain(state, stub)
    log = (state / "drain.log").read_text()
    assert "WRITER RESPAWN (writer_drift)" in log
    assert "categories=[poisoned]" in log
    assert (state / "poison-reflections.jsonl").read_text().strip()


# ---------- /reflect:cost writer-health view ----------

def test_reflect_cost_groups_by_writer_class(tmp_path):
    state = tmp_path / "state"
    state.mkdir()
    rows = [
        {"ts": "2099-01-01T00:00:00Z", "day": "2099-01-01", "entries": 1,
         "transcript": "/t/a.jsonl", "outcome": "ok", "model": "sonnet",
         "tokens": 100, "writer_class": "valid"},
        {"ts": "2099-01-01T00:01:00Z", "day": "2099-01-01", "entries": 1,
         "transcript": "/t/b.jsonl", "outcome": "poison_writer_drift",
         "model": "sonnet", "tokens": 50, "writer_class": "prose"},
    ]
    (state / "drain-cost.jsonl").write_text(
        "\n".join(json.dumps(r) for r in rows) + "\n"
    )
    cp = subprocess.run(
        [sys.executable, str(SCRIPTS / "reflect_cost.py"),
         "--since", "300000d", "--by", "writer", "--json", "--state-dir", str(state)],
        capture_output=True, text=True, timeout=30,
    )
    assert cp.returncode == 0, cp.stderr
    agg = json.loads(cp.stdout)
    assert set(agg) == {"valid", "prose"}
    assert agg["valid"]["tokens"] == 100
    assert agg["prose"]["tokens"] == 50


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
