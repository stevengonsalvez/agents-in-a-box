# ABOUTME: Unit tests for reflect_kb.errors — append/dedupe/ack/cap/corruption.
# ABOUTME: Isolates state dir via REFLECT_STATE_DIR + tmp_path; CLI smoke included.
import json
import os
import subprocess
import sys
from datetime import datetime, timezone, timedelta
from pathlib import Path

import pytest


@pytest.fixture(autouse=True)
def _isolated_state_dir(tmp_path, monkeypatch):
    """Point REFLECT_STATE_DIR at tmp_path and reload module so paths re-resolve."""
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path))
    # Re-import fresh so any cached module-level state (none today, but safe) resets.
    import importlib

    import reflect_kb.errors as errors_mod

    importlib.reload(errors_mod)
    yield errors_mod


def _read(state_dir: Path) -> dict:
    p = state_dir / "errors.json"
    return json.loads(p.read_text())


def test_identical_appends_dedupe_to_one_record_with_count_two(_isolated_state_dir, tmp_path):
    errors = _isolated_state_dir
    id1 = errors.append(severity="warn", source="parse", kind="sidecar_typeerror", message="bad")
    id2 = errors.append(severity="warn", source="parse", kind="sidecar_typeerror", message="bad")

    assert id1 == id2
    doc = _read(tmp_path)
    assert len(doc["errors"]) == 1
    assert doc["errors"][0]["count"] == 2
    assert doc["errors"][0]["acked"] is False


def test_dedupe_window_expired_creates_new_record(_isolated_state_dir, tmp_path):
    errors = _isolated_state_dir
    errors.append(severity="warn", source="parse", kind="sidecar_typeerror", message="bad")

    # Force the stored ts to be older than 24h.
    doc = _read(tmp_path)
    old_ts = (datetime.now(timezone.utc) - timedelta(seconds=errors.DEDUPE_WINDOW_SEC + 60)).isoformat(
        timespec="seconds"
    ).replace("+00:00", "Z")
    doc["errors"][0]["ts"] = old_ts
    (tmp_path / "errors.json").write_text(json.dumps(doc))

    errors.append(severity="warn", source="parse", kind="sidecar_typeerror", message="bad")
    doc2 = _read(tmp_path)
    # Two records now: same id but separate entries (insert path took it because age > window).
    # The schema stores them as two list entries even with the same id.
    assert len(doc2["errors"]) == 2


def test_ack_all_flips_unacked_to_zero(_isolated_state_dir, tmp_path):
    errors = _isolated_state_dir
    errors.append(severity="warn", source="parse", kind="k1", message="m1")
    errors.append(severity="error", source="drain", kind="k2", message="m2")
    errors.append(severity="error", source="drain", kind="k3", message="m3")

    assert errors.count_unacked() == 3
    flipped = errors.ack(ids=None)
    assert flipped == 3
    assert errors.count_unacked() == 0


def test_max_records_cap_drops_oldest(_isolated_state_dir, tmp_path):
    errors = _isolated_state_dir
    # Append MAX_RECORDS + 5 unique records.
    total = errors.MAX_RECORDS + 5
    for i in range(total):
        errors.append(
            severity="warn", source="test", kind=f"k{i}", message=f"msg{i}"
        )
    doc = _read(tmp_path)
    assert len(doc["errors"]) == errors.MAX_RECORDS
    # Newest insertions are at the head; oldest indices should have been dropped.
    head_kinds = {r["kind"] for r in doc["errors"][:5]}
    assert f"k{total - 1}" in head_kinds  # most recent retained


def test_corrupt_file_recovers_silently(_isolated_state_dir, tmp_path):
    errors = _isolated_state_dir
    # Pre-write garbage.
    (tmp_path / "errors.json").write_text("not json {[}")

    id_ = errors.append(severity="warn", source="parse", kind="recovery", message="ok")
    assert id_.startswith("err-")
    doc = _read(tmp_path)
    assert len(doc["errors"]) == 1
    assert doc["errors"][0]["kind"] == "recovery"


def test_cli_smoke_append_count_ack(_isolated_state_dir, tmp_path):
    env = os.environ.copy()
    env["REFLECT_STATE_DIR"] = str(tmp_path)

    src_root = Path(__file__).resolve().parent.parent / "src"
    env["PYTHONPATH"] = str(src_root) + os.pathsep + env.get("PYTHONPATH", "")

    def run(*args):
        return subprocess.run(
            [sys.executable, "-m", "reflect_kb.errors", *args],
            env=env, capture_output=True, text=True, check=True,
        )

    r1 = run("append", "--source", "test", "--kind", "smoke", "--message", "hi")
    assert r1.stdout.strip().startswith("err-")

    r2 = run("count")
    assert r2.stdout.strip() == "1"

    r3 = run("ack")
    assert r3.stdout.strip() == "1"

    r4 = run("count")
    assert r4.stdout.strip() == "0"
