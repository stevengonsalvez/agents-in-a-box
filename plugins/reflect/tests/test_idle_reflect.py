# ABOUTME: Regression tests for port SG3 — session idle trigger for natural
# ABOUTME: reflection. Pins the idle sweep (reflect_gate.idle_sweep + the
# ABOUTME: idle_reflect.sh hook), the speculative down-rank in recall, and the
# ABOUTME: resume-after-idle no-double-process guarantees.
"""Port SG3: session idle trigger for natural reflection.

Daemon (launchd com.reflect.idle.plist → hooks/idle_reflect.sh →
reflect_gate.py --idle-sweep) watches ~/.claude/projects/*/*.jsonl transcript
mtimes; sessions quiet past the idle threshold are enqueued with
trigger='idle'. The drain prompt tags their learnings 'speculative' and
recall down-ranks that tag.

Acceptance bullets pinned here:
  1. idle threshold configurable (param + REFLECT_IDLE_THRESHOLD_SEC env)
  2. speculative-tagged reflections rank lower in recall
  3. resume-after-idle does not double-process
"""

from __future__ import annotations

import importlib
import json
import os
import stat
import subprocess
import sys
import time
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
SCRIPTS = PLUGIN_ROOT / "scripts"
GATE = SCRIPTS / "reflect_gate.py"
IDLE_HOOK = PLUGIN_ROOT / "hooks" / "idle_reflect.sh"
DRAIN = PLUGIN_ROOT / "hooks" / "reflect-drain-bg.sh"
STOP = PLUGIN_ROOT / "hooks" / "stop_reflect.py"
RECALL_SCRIPTS = PLUGIN_ROOT / "skills" / "recall" / "scripts"
PLIST = PLUGIN_ROOT / "launchd" / "com.reflect.idle.plist"

sys.path.insert(0, str(SCRIPTS))
sys.path.insert(0, str(RECALL_SCRIPTS))

import reflect_gate  # noqa: E402
import recall as recall_mod  # noqa: E402


# ── fixtures ────────────────────────────────────────────────────────────────

def _signal_transcript(path: Path) -> Path:
    """Minimal transcript carrying a correction signal (passes the gate)."""
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w") as fh:
        fh.write(json.dumps({"message": {
            "role": "user",
            "content": "No, never use var here. The root cause was a missing index.",
        }}) + "\n")
        fh.write(json.dumps({"message": {
            "role": "assistant",
            "content": "Understood — switching to const and adding the index.",
        }}) + "\n")
    return path


def _set_mtime(path: Path, age_sec: float, now: float) -> None:
    ts = now - age_sec
    os.utime(path, (ts, ts))


def _projects_root(tmp_path: Path) -> Path:
    root = tmp_path / "projects"
    (root / "-Users-x-dev-proj").mkdir(parents=True)
    return root


def _queue_entries(state_dir: Path) -> list[dict]:
    q = state_dir / "pending_reflections.jsonl"
    if not q.exists():
        return []
    return [json.loads(ln) for ln in q.read_text().splitlines() if ln.strip()]


def _sweep(root: Path, state: Path, now: float, **kw) -> dict:
    state.mkdir(parents=True, exist_ok=True)
    return reflect_gate.idle_sweep(
        root,
        state / "pending_reflections.jsonl",
        state / "drain-cost.jsonl",
        state / "idle-state.json",
        now=now,
        **kw,
    )


# ── acceptance 1: idle threshold configurable ───────────────────────────────

def test_scan_idle_window_default_threshold(tmp_path):
    now = time.time()
    root = _projects_root(tmp_path)
    proj = root / "-Users-x-dev-proj"
    active = _signal_transcript(proj / "active.jsonl")
    idle = _signal_transcript(proj / "idle.jsonl")
    ancient = _signal_transcript(proj / "ancient.jsonl")
    _set_mtime(active, 30, now)            # still being written
    _set_mtime(idle, 700, now)             # quiet past the 600s default
    _set_mtime(ancient, 3 * 86_400, now)   # dead long before the daemon ran

    found = [p for p, _ in reflect_gate.scan_idle_transcripts(root, now=now)]
    assert found == [idle], (
        "default window must catch ONLY the 600s-quiet transcript "
        f"(got {[p.name for p in found]})"
    )


def test_threshold_configurable_via_param(tmp_path):
    now = time.time()
    root = _projects_root(tmp_path)
    t = _signal_transcript(root / "-Users-x-dev-proj" / "s1.jsonl")
    _set_mtime(t, 120, now)
    # Default 600s: not idle yet.
    assert reflect_gate.scan_idle_transcripts(root, now=now) == []
    # Tightened threshold: idle.
    found = reflect_gate.scan_idle_transcripts(root, threshold_sec=60, now=now)
    assert [p for p, _ in found] == [t]


def test_threshold_configurable_via_env_through_cli(tmp_path):
    """REFLECT_IDLE_THRESHOLD_SEC reaches the sweep via the CLI env fallback."""
    now = time.time()
    root = _projects_root(tmp_path)
    t = _signal_transcript(root / "-Users-x-dev-proj" / "s1.jsonl")
    _set_mtime(t, 120, now)
    state = tmp_path / "state"
    state.mkdir()
    env = dict(os.environ)
    env.update({
        "REFLECT_STATE_DIR": str(state),
        "REFLECT_IDLE_PROJECTS_ROOT": str(root),
        "REFLECT_IDLE_THRESHOLD_SEC": "60",
    })
    cp = subprocess.run(
        [sys.executable, str(GATE), "--idle-sweep"],
        env=env, capture_output=True, text=True, timeout=60,
    )
    assert cp.returncode == 0, cp.stderr
    summary = json.loads(cp.stdout)
    assert summary["enqueued"] == 1
    entries = _queue_entries(state)
    assert len(entries) == 1 and entries[0]["transcript_path"] == str(t)


def test_idle_entry_shape(tmp_path):
    """Queue entry carries trigger='idle', speculative=True, session_id=stem."""
    now = time.time()
    root = _projects_root(tmp_path)
    t = _signal_transcript(root / "-Users-x-dev-proj" / "abc-123.jsonl")
    _set_mtime(t, 700, now)
    state = tmp_path / "state"
    summary = _sweep(root, state, now)
    assert summary["enqueued"] == 1
    (entry,) = _queue_entries(state)
    assert entry["trigger"] == "idle"
    assert entry["speculative"] is True
    assert entry["session_id"] == "abc-123"
    assert entry["transcript_path"] == str(t)


def test_no_signal_transcript_not_enqueued(tmp_path):
    """The idle sweep still respects the $0 signal gate — quiet+clean = skip."""
    now = time.time()
    root = _projects_root(tmp_path)
    clean = root / "-Users-x-dev-proj" / "clean.jsonl"
    clean.write_text(json.dumps({"message": {
        "role": "user", "content": "Morning. Summarize the attached document.",
    }}) + "\n")
    _set_mtime(clean, 700, now)
    state = tmp_path / "state"
    summary = _sweep(root, state, now)
    assert summary["enqueued"] == 0 and _queue_entries(state) == []


def test_max_per_sweep_caps_enqueues(tmp_path):
    now = time.time()
    root = _projects_root(tmp_path)
    proj = root / "-Users-x-dev-proj"
    for i in range(4):
        _set_mtime(_signal_transcript(proj / f"s{i}.jsonl"), 700 + i, now)
    state = tmp_path / "state"
    summary = _sweep(root, state, now, max_per_sweep=2)
    assert summary["enqueued"] == 2
    assert len(_queue_entries(state)) == 2


# ── acceptance 3: resume-after-idle does not double-process ─────────────────

def test_still_idle_session_not_reenqueued(tmp_path):
    """Two sweeps over the same idle period → exactly one queue entry."""
    now = time.time()
    root = _projects_root(tmp_path)
    t = _signal_transcript(root / "-Users-x-dev-proj" / "s1.jsonl")
    _set_mtime(t, 700, now)
    state = tmp_path / "state"
    assert _sweep(root, state, now)["enqueued"] == 1
    assert _sweep(root, state, now + 300)["enqueued"] == 0
    assert len(_queue_entries(state)) == 1, "still-idle session double-enqueued"


def test_resume_after_processed_idle_is_not_reprocessed(tmp_path):
    """Idle entry drained to a terminal outcome → a resume + re-idle of the
    same transcript must NOT enqueue it again (already_processed dedup)."""
    now = time.time()
    root = _projects_root(tmp_path)
    t = _signal_transcript(root / "-Users-x-dev-proj" / "s1.jsonl")
    _set_mtime(t, 700, now)
    state = tmp_path / "state"
    assert _sweep(root, state, now)["enqueued"] == 1

    # Simulate the drain: entry processed (terminal "ok"), queue rewritten.
    (state / "drain-cost.jsonl").write_text(json.dumps({
        "ts": "t", "day": "2026-06-10", "entries": 1,
        "transcript": str(t), "outcome": "ok",
    }) + "\n")
    (state / "pending_reflections.jsonl").write_text("")

    # Resume (mtime bumps) then idle again.
    later = now + 3600
    _set_mtime(t, 700, later)
    summary = _sweep(root, state, later)
    assert summary["enqueued"] == 0, "resume-after-idle was double-processed"
    assert _queue_entries(state) == []


def test_stop_after_idle_does_not_double_enqueue(tmp_path):
    """Session goes idle (idle entry queued), then resumes and ends with Stop:
    the Stop hook's session-id dedup must not add a second entry."""
    now = time.time()
    root = _projects_root(tmp_path)
    t = _signal_transcript(root / "-Users-x-dev-proj" / "sess-42.jsonl")
    _set_mtime(t, 700, now)
    state = tmp_path / "state"
    assert _sweep(root, state, now)["enqueued"] == 1

    env = dict(os.environ)
    env["REFLECT_STATE_DIR"] = str(state)
    payload = {"session_id": "sess-42", "transcript_path": str(t),
               "trigger": "stop", "cwd": "/"}
    subprocess.run([sys.executable, str(STOP)], input=json.dumps(payload),
                   text=True, capture_output=True, env=env, timeout=60)
    entries = _queue_entries(state)
    assert len(entries) == 1, "Stop-after-idle double-enqueued the session"
    assert entries[0]["trigger"] == "idle"


def test_idle_after_queued_by_precompact_is_deduped(tmp_path):
    """A transcript already queued (e.g. by PreCompact) is skipped by the
    sweep's already_queued dedup — the producers can't double-queue."""
    now = time.time()
    root = _projects_root(tmp_path)
    t = _signal_transcript(root / "-Users-x-dev-proj" / "s9.jsonl")
    _set_mtime(t, 700, now)
    state = tmp_path / "state"
    state.mkdir()
    (state / "pending_reflections.jsonl").write_text(json.dumps({
        "ts": "t", "session_id": "s9", "transcript_path": str(t),
        "trigger": "auto", "cwd": "/",
    }) + "\n")
    summary = _sweep(root, state, now)
    assert summary["enqueued"] == 0
    assert len(_queue_entries(state)) == 1


# ── acceptance 2: speculative-tagged reflections rank lower in recall ───────

def _learning(id_: str, tags: list[str]) -> "recall_mod.Learning":
    return recall_mod.Learning(
        chunk_text=f"learning {id_}",
        frontmatter={"id": id_, "confidence": "HIGH", "tags": tags},
    )


def test_speculative_ranks_below_identical_twin():
    normal = _learning("l-normal", ["db"])
    spec = _learning("l-spec", ["db", "speculative"])
    ranked, scores = recall_mod.rerank_with_scores([spec, normal])
    assert [l.id for l in ranked] == ["l-normal", "l-spec"]
    ratio = (scores[recall_mod._learning_key(spec)]
             / scores[recall_mod._learning_key(normal)])
    expected = 1.0 - recall_mod.SPECULATIVE_ALPHA / 2.0
    assert ratio == pytest.approx(expected), (
        "speculative penalty must be exactly the bounded-boost floor"
    )


def test_speculative_tag_match_is_case_insensitive():
    assert recall_mod.speculative_norm(["Speculative"]) == 0.0
    assert recall_mod.speculative_norm([" SPECULATIVE "]) == 0.0


def test_non_speculative_score_exactly_unchanged():
    """Untagged learnings sit at the neutral norm — multiplier EXACTLY 1.0,
    so pre-SG3 scores are bit-identical."""
    assert recall_mod.speculative_norm(["db", "postgres"]) == 0.5
    assert recall_mod.bounded_boost(0.5, recall_mod.SPECULATIVE_ALPHA) == 1.0


def test_speculative_boost_stays_in_bounded_range():
    lo = recall_mod.bounded_boost(
        recall_mod.speculative_norm(["speculative"]), recall_mod.SPECULATIVE_ALPHA)
    assert lo == pytest.approx(1.0 - recall_mod.SPECULATIVE_ALPHA / 2.0)
    assert 0.0 < lo < 1.0


def test_speculative_alpha_env_zero_disables(monkeypatch):
    monkeypatch.setenv("RECALL_SPECULATIVE_ALPHA", "0")
    mod = importlib.reload(recall_mod)
    try:
        assert mod.SPECULATIVE_ALPHA == 0.0
        spec = mod.Learning(chunk_text="s", frontmatter={
            "id": "s", "confidence": "HIGH", "tags": ["speculative"]})
        norm = mod.Learning(chunk_text="n", frontmatter={
            "id": "n", "confidence": "HIGH", "tags": []})
        _, scores = mod.rerank_with_scores([spec, norm])
        assert scores[mod._learning_key(spec)] == pytest.approx(
            scores[mod._learning_key(norm)])
    finally:
        monkeypatch.delenv("RECALL_SPECULATIVE_ALPHA")
        importlib.reload(recall_mod)


# ── drain: idle entries get the speculative prompt addendum ─────────────────

_OK_ENVELOPE = json.dumps({
    "is_error": False, "result": "captured", "total_cost_usd": 0.01,
    "num_turns": 1, "usage": {"input_tokens": 10, "output_tokens": 10},
})


def _stub_claude(tmp_path: Path) -> tuple[Path, Path]:
    """Stub `claude` that records its argv (the prompt) and emits a valid
    result envelope."""
    record = tmp_path / "prompt-record.txt"
    stub = tmp_path / "stub-claude"
    stub.write_text(
        "#!/usr/bin/env bash\n"
        f'printf \'%s\\n\' "$@" >> "{record}"\n'
        f"cat <<'EOF'\n{_OK_ENVELOPE}\nEOF\n"
    )
    stub.chmod(stub.stat().st_mode | stat.S_IEXEC)
    return stub, record


def _run_drain(state_dir: Path, stub: Path) -> None:
    env = dict(os.environ)
    env.update({
        "REFLECT_STATE_DIR": str(state_dir),
        "REFLECT_DRAIN_DRY_RUN": "0",
        "REFLECT_DRAIN_CLAUDE_BIN": str(stub),
        "REFLECT_DRAIN_SKIP_REINDEX": "1",
        "REFLECT_DRAIN_DEBOUNCE_SEC": "0",
        "REFLECT_DRAIN_CASCADE": "0",
        "REFLECT_QUIET_INSTALL_WARNING": "1",
    })
    subprocess.run(["bash", str(DRAIN)], env=env, capture_output=True,
                   text=True, timeout=60)


def _seed_drain_queue(state_dir: Path, trigger: str) -> None:
    state_dir.mkdir(parents=True, exist_ok=True)
    transcript = state_dir / f"transcript-{trigger}.jsonl"
    transcript.write_text("{}\n")
    (state_dir / "pending_reflections.jsonl").write_text(json.dumps({
        "ts": "t", "session_id": "s1", "transcript_path": str(transcript),
        "trigger": trigger, "cwd": "/",
    }) + "\n")


def test_drain_tags_idle_entries_speculative(tmp_path):
    stub, record = _stub_claude(tmp_path)
    state = tmp_path / "state"
    _seed_drain_queue(state, "idle")
    _run_drain(state, stub)
    prompt = record.read_text()
    assert "speculative" in prompt, "idle drain prompt lost the speculative tag"
    assert "cap confidence at MEDIUM" in prompt


def test_drain_stop_entries_not_tagged_speculative(tmp_path):
    stub, record = _stub_claude(tmp_path)
    state = tmp_path / "state"
    _seed_drain_queue(state, "stop")
    _run_drain(state, stub)
    prompt = record.read_text()
    assert "speculative" not in prompt, "stop drain prompt wrongly speculative"


# ── hook + plist plumbing ────────────────────────────────────────────────────

def test_idle_hook_syntax_valid():
    cp = subprocess.run(["bash", "-n", str(IDLE_HOOK)],
                        capture_output=True, text=True)
    assert cp.returncode == 0, cp.stderr


@pytest.mark.parametrize("switch", ["REFLECT_DISABLED", "REFLECT_IDLE_DISABLED"])
def test_idle_hook_kill_switches_are_total_noop(tmp_path, switch):
    state = tmp_path / "state"
    env = dict(os.environ)
    env.update({"REFLECT_STATE_DIR": str(state), switch: "1"})
    cp = subprocess.run(["bash", str(IDLE_HOOK)], env=env,
                        capture_output=True, text=True, timeout=30)
    assert cp.returncode == 0
    assert not (state / "idle.log").exists(), "kill switch did work anyway"


def test_idle_hook_end_to_end_enqueues(tmp_path):
    now = time.time()
    root = _projects_root(tmp_path)
    t = _signal_transcript(root / "-Users-x-dev-proj" / "s1.jsonl")
    _set_mtime(t, 700, now)
    state = tmp_path / "state"
    env = dict(os.environ)
    env.update({
        "REFLECT_STATE_DIR": str(state),
        "REFLECT_IDLE_PROJECTS_ROOT": str(root),
    })
    cp = subprocess.run(["bash", str(IDLE_HOOK)], env=env,
                        capture_output=True, text=True, timeout=60)
    assert cp.returncode == 0
    (entry,) = _queue_entries(state)
    assert entry["trigger"] == "idle" and entry["transcript_path"] == str(t)
    assert "idle sweep" in (state / "idle.log").read_text()


def test_launchd_plist_well_formed():
    import plistlib
    raw = PLIST.read_text().replace("{{PLUGIN_ROOT}}", "/opt/reflect")
    data = plistlib.loads(raw.encode())
    assert data["Label"] == "com.reflect.idle"
    assert data["ProgramArguments"][-1].endswith("hooks/idle_reflect.sh")
    assert data["StartInterval"] <= reflect_gate.DEFAULT_IDLE_THRESHOLD_SEC, (
        "timer must tick at least as often as the idle threshold"
    )


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
