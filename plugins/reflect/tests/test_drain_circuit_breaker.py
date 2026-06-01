"""Behavior tests for the reflect-drain-bg.sh circuit breaker (W1).

Covers the guards added after the 2026-05-31 incident where a single drain
ran 223 Opus turns / 41.5M tokens because the only bound was a 600s wall-clock:

  * REFLECT_DISABLED hard kill switch (no-op, not even a log)
  * atomic mkdir lock (live owner blocks; dead owner reclaimed)
  * debounce window collapses a burst of session starts to one drain
  * DRY_RUN is side-effect-free (must NOT trigger a real reindex)

These shell out to the script with an isolated REFLECT_STATE_DIR so nothing
touches the real ~/.reflect or the global KB. Token-budget poison and the
--model flag are exercised against the live `claude` binary, so they are
covered by the manual harness, not here.
"""

from __future__ import annotations

import os
import signal
import subprocess
import time
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
DRAIN = PLUGIN_ROOT / "hooks" / "reflect-drain-bg.sh"


def _make_queue(state_dir: Path) -> Path:
    """Seed an isolated state dir with one queue entry pointing at a real file."""
    state_dir.mkdir(parents=True, exist_ok=True)
    transcript = state_dir / "fake-transcript.jsonl"
    transcript.write_text("{}\n")
    queue = state_dir / "pending_reflections.jsonl"
    queue.write_text(
        '{"ts":"t","session_id":"s1","transcript_path":"%s",'
        '"trigger":"stop","cwd":"/"}\n' % transcript
    )
    return queue


def _run(state_dir: Path, **env_overrides) -> subprocess.CompletedProcess:
    env = dict(os.environ)
    env.update(
        {
            "REFLECT_STATE_DIR": str(state_dir),
            "REFLECT_DRAIN_DRY_RUN": "1",
            "REFLECT_DRAIN_SKIP_REINDEX": "1",
            "REFLECT_DRAIN_DEBOUNCE_SEC": "0",
            # Isolate circuit-breaker mechanics from the W4 cascade gate, which
            # would otherwise skip the synthetic `{}` transcript as no-signal.
            "REFLECT_DRAIN_CASCADE": "0",
        }
    )
    env.update({k: str(v) for k, v in env_overrides.items()})
    return subprocess.run(
        ["bash", str(DRAIN)], env=env, capture_output=True, text=True, timeout=60
    )


def test_syntax_valid():
    cp = subprocess.run(["bash", "-n", str(DRAIN)], capture_output=True, text=True)
    assert cp.returncode == 0, cp.stderr


def test_kill_switch_is_total_noop(tmp_path):
    state = tmp_path / "state"
    _make_queue(state)
    _run(state, REFLECT_DISABLED="1")
    # Not even a log file — the switch is honoured before any work.
    assert not (state / "drain.log").exists()
    # Queue untouched.
    assert (state / "pending_reflections.jsonl").read_text().strip()


def test_dry_run_processes_without_reindex(tmp_path):
    state = tmp_path / "state"
    _make_queue(state)
    _run(state)
    log = (state / "drain.log").read_text()
    assert "drain start" in log
    assert "DRY_RUN=1" in log
    # A dry run must have zero side effects beyond logging.
    assert "reindex" not in log
    # Entry was removed from the queue after a successful (dry) process.
    assert (state / "pending_reflections.jsonl").read_text().strip() == ""


def test_debounce_blocks_immediate_rerun(tmp_path):
    state = tmp_path / "state"
    _make_queue(state)
    # First run with a long window stamps the debounce file.
    _run(state, REFLECT_DRAIN_DEBOUNCE_SEC="600")
    _make_queue(state)
    _run(state, REFLECT_DRAIN_DEBOUNCE_SEC="600")
    log = (state / "drain.log").read_text()
    assert "debounce:" in log
    # The second run must not have processed the re-seeded entry.
    assert (state / "pending_reflections.jsonl").read_text().strip()


def test_live_lock_owner_blocks_new_drain(tmp_path):
    state = tmp_path / "state"
    _make_queue(state)
    # Hold the lock with a live process.
    holder = subprocess.Popen(["sleep", "30"])
    try:
        lock = state / "drain.lock.d"
        lock.mkdir(parents=True)
        (lock / "pid").write_text(str(holder.pid))
        _run(state)
        log = (state / "drain.log").read_text()
        assert f"another drain is running (pid={holder.pid})" in log
        # Queue untouched — the blocked drain did no work.
        assert (state / "pending_reflections.jsonl").read_text().strip()
    finally:
        holder.send_signal(signal.SIGTERM)
        holder.wait(timeout=5)


def test_stale_lock_is_reclaimed(tmp_path):
    state = tmp_path / "state"
    _make_queue(state)
    lock = state / "drain.lock.d"
    lock.mkdir(parents=True)
    (lock / "pid").write_text("999999")  # almost certainly dead
    _run(state)
    log = (state / "drain.log").read_text()
    assert "stale lock detected" in log
    # After reclaiming, it processed the entry.
    assert (state / "pending_reflections.jsonl").read_text().strip() == ""


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
