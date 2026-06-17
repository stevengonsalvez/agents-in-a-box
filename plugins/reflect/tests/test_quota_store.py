# ABOUTME: Regression tests for the subscription-quota writer gate (port M3, pattern from claude-mem RateLimitStore).
# ABOUTME: Pins ingest+TTL persistence, the surpassedThreshold abort rule, quota status output, defer/replay, zero-API-call gate.
"""Behavior tests for quota_store.py + the drain quota gate (M3).

Acceptance criteria pinned here:

  * Quota store ingests rate_limit events from the SDK stream and persists
    to disk with a TTL.
  * Writer aborts when surpassedThreshold && !isUsingOverage.
  * `quota status` prints the four window fields and whether the gate is
    open or closed (also surfaced via reflect_cost.py --quota).
  * Deferred-write marker is replayable once the quota recovers (queue
    entries are never consumed on defer).
  * No additional API calls are issued purely to check quota (a closed gate
    never invokes the claude binary; `check` reads disk only).

Drain integration shells out to reflect-drain-bg.sh with an isolated
REFLECT_STATE_DIR and a fake `claude` recorder binary, mirroring
test_drain_circuit_breaker.py.
"""

from __future__ import annotations

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
DRAIN = PLUGIN_ROOT / "hooks" / "reflect-drain-bg.sh"
QUOTA = SCRIPTS / "quota_store.py"
COST = SCRIPTS / "reflect_cost.py"

sys.path.insert(0, str(SCRIPTS))

import quota_store  # noqa: E402


# ── Helpers ───────────────────────────────────────────────────────────────────

def _seed_state(sd: Path, entry: dict, bucket: str = "five_hour",
                observed_at: float | None = None) -> None:
    sd.mkdir(parents=True, exist_ok=True)
    e = dict(entry)
    e["observed_at"] = time.time() if observed_at is None else observed_at
    existing = {}
    path = sd / "quota-state.json"
    if path.exists():
        existing = json.loads(path.read_text())
    existing[bucket] = e
    path.write_text(json.dumps(existing))


def _quota_cli(sd: Path, *args: str, stdin: str = "") -> subprocess.CompletedProcess:
    env = dict(os.environ)
    env["ANTHROPIC_API_KEY"] = ""  # subscription auth: the gate applies
    return subprocess.run(
        [sys.executable, str(QUOTA), *args, "--state-dir", str(sd)],
        input=stdin, env=env, capture_output=True, text=True, timeout=30,
    )


def _make_queue(state_dir: Path, n: int = 1) -> Path:
    state_dir.mkdir(parents=True, exist_ok=True)
    lines = []
    for i in range(n):
        transcript = state_dir / f"fake-transcript-{i}.jsonl"
        transcript.write_text("{}\n")
        lines.append(json.dumps({
            "ts": "t", "session_id": f"s{i}",
            "transcript_path": str(transcript), "trigger": "stop", "cwd": "/",
        }))
    queue = state_dir / "pending_reflections.jsonl"
    queue.write_text("\n".join(lines) + "\n")
    return queue


def _fake_claude(tmp_path: Path, body: str) -> Path:
    """A fake `claude` binary: records each invocation, prints `body`."""
    bin_path = tmp_path / "fake-claude"
    calls = tmp_path / "claude-calls.log"
    bin_path.write_text(
        "#!/usr/bin/env bash\n"
        f'echo "called" >> "{calls}"\n'
        f"cat <<'EOF'\n{body}\nEOF\n"
    )
    bin_path.chmod(bin_path.stat().st_mode | stat.S_IEXEC)
    return bin_path


def _run_drain(state_dir: Path, **env_overrides) -> subprocess.CompletedProcess:
    env = dict(os.environ)
    env.update({
        "REFLECT_STATE_DIR": str(state_dir),
        "REFLECT_DRAIN_SKIP_REINDEX": "1",
        "REFLECT_DRAIN_DEBOUNCE_SEC": "0",
        "REFLECT_DRAIN_CASCADE": "0",
        "ANTHROPIC_API_KEY": "",  # subscription auth: the quota gate applies
    })
    env.update({k: str(v) for k, v in env_overrides.items()})
    return subprocess.run(
        ["bash", str(DRAIN)], env=env, capture_output=True, text=True, timeout=60
    )


# ── Ingest + TTL persistence ──────────────────────────────────────────────────

def test_ingest_sdk_stream_event_persists_to_disk(tmp_path):
    """rate_limit system events from the SDK stream land in quota-state.json."""
    event = json.dumps({
        "type": "system", "subtype": "rate_limit",
        "rate_limit_info": {
            "status": "allowed_warning", "rateLimitType": "five_hour",
            "utilization": 0.91, "surpassedThreshold": 0.8,
            "isUsingOverage": True, "resetsAt": 9999999999999,
        },
    })
    cp = _quota_cli(tmp_path, "ingest", stdin=event)
    assert cp.returncode == 0, cp.stderr
    assert json.loads(cp.stdout)["ingested"] == 1
    state = json.loads((tmp_path / "quota-state.json").read_text())
    entry = state["five_hour"]
    assert entry["status"] == "allowed_warning"
    assert entry["utilization"] == 0.91
    assert entry["surpassedThreshold"] == 0.8
    assert entry["isUsingOverage"] is True
    assert entry["observed_at"] > 0


def test_ingest_result_envelope_and_stream_json_lines(tmp_path):
    """Telemetry is found inside a result envelope AND in stream-json lines."""
    envelope = json.dumps({
        "type": "result", "is_error": False, "result": "ok",
        "rate_limit_info": {"status": "allowed", "rateLimitType": "seven_day",
                            "utilization": 0.4},
    })
    assert quota_store.parse_output(envelope)[0]["rateLimitType"] == "seven_day"
    stream = "\n".join([
        json.dumps({"type": "system", "subtype": "init"}),
        json.dumps({"type": "system", "subtype": "rate_limit",
                    "rate_limit_info": {"status": "allowed",
                                        "rateLimitType": "seven_day_opus",
                                        "utilization": 0.2}}),
        json.dumps({"type": "result", "is_error": False}),
    ])
    infos = quota_store.parse_output(stream)
    assert any(i.get("rateLimitType") == "seven_day_opus" for i in infos)


def test_ttl_expiry_drops_stale_snapshot_and_reopens_gate(tmp_path):
    """Entries older than the TTL are dropped on read — the gate fails open."""
    _seed_state(tmp_path, {"status": "rejected"}, bucket="five_hour",
                observed_at=time.time() - 7200)  # 2h old vs 3600s TTL
    state = quota_store.load_state(tmp_path, ttl=3600)
    assert state == {}
    decision = quota_store.should_abort(state, api_key_auth=False)
    assert decision.abort is False
    # A fresh snapshot within the TTL is retained.
    _seed_state(tmp_path, {"status": "rejected"}, bucket="five_hour")
    state = quota_store.load_state(tmp_path, ttl=3600)
    assert "five_hour" in state


def test_last_write_wins_per_bucket(tmp_path):
    quota_store.ingest_infos(tmp_path, [
        {"rateLimitType": "five_hour", "utilization": 0.5, "status": "allowed"}])
    quota_store.ingest_infos(tmp_path, [
        {"rateLimitType": "five_hour", "utilization": 0.9,
         "status": "allowed_warning"}])
    state = quota_store.load_state(tmp_path)
    assert state["five_hour"]["utilization"] == 0.9
    assert len(state) == 1


def test_stderr_429_fallback_synthesizes_rejected(tmp_path):
    """No envelope telemetry + 429/529 on stderr => default bucket rejected."""
    stderr_file = tmp_path / "stderr.txt"
    stderr_file.write_text("API Error: 429 rate_limit_error: rate limited\n")
    cp = _quota_cli(tmp_path, "ingest", "--stderr-file", str(stderr_file),
                    stdin="")
    assert json.loads(cp.stdout)["ingested"] == 1
    state = quota_store.load_state(tmp_path)
    assert state["default"]["status"] == "rejected"
    decision = quota_store.should_abort(state, api_key_auth=False)
    assert decision.abort is True


# ── The abort rule ────────────────────────────────────────────────────────────

def test_abort_when_surpassed_threshold_without_overage():
    """The acceptance rule: surpassedThreshold && !isUsingOverage => abort."""
    state = {"five_hour": {"status": "allowed_warning",
                           "surpassedThreshold": 0.8,
                           "isUsingOverage": False, "utilization": 0.82}}
    decision = quota_store.should_abort(state, api_key_auth=False)
    assert decision.abort is True
    assert decision.window == "five_hour"
    assert "surpassedThreshold" in decision.reason


def test_no_abort_when_overage_absorbs_the_spill():
    state = {"five_hour": {"status": "allowed_warning",
                           "surpassedThreshold": 0.8,
                           "isUsingOverage": True, "utilization": 0.82}}
    assert quota_store.should_abort(state, api_key_auth=False).abort is False


def test_abort_on_provider_rejection_and_utilization_ceiling():
    rejected = {"seven_day": {"status": "rejected"}}
    assert quota_store.should_abort(rejected, api_key_auth=False).abort is True
    hot = {"seven_day_sonnet": {"status": "allowed", "utilization": 0.99}}
    assert quota_store.should_abort(hot, api_key_auth=False).abort is True
    cool = {"seven_day_sonnet": {"status": "allowed", "utilization": 0.5}}
    assert quota_store.should_abort(cool, api_key_auth=False).abort is False


def test_api_key_auth_is_exempt():
    """Per-call billing = the user authorized the spend; never abort."""
    state = {"five_hour": {"status": "rejected"}}
    assert quota_store.should_abort(state, api_key_auth=True).abort is False


# ── Status surface ────────────────────────────────────────────────────────────

def test_status_prints_four_windows_and_gate_open(tmp_path):
    cp = _quota_cli(tmp_path, "status")
    assert cp.returncode == 0, cp.stderr
    out = cp.stdout
    for window in ("five_hour", "seven_day", "seven_day_opus", "seven_day_sonnet"):
        assert window in out
    assert "quota gate: OPEN" in out


def test_status_prints_gate_closed_with_reason(tmp_path):
    _seed_state(tmp_path, {"status": "allowed_warning",
                           "surpassedThreshold": 0.8, "isUsingOverage": False})
    cp = _quota_cli(tmp_path, "status")
    assert "quota gate: CLOSED" in cp.stdout
    assert "five_hour" in cp.stdout
    payload = json.loads(_quota_cli(tmp_path, "status", "--json").stdout)
    assert payload["gate"] == "closed"
    assert set(payload["windows"]) == {
        "five_hour", "seven_day", "seven_day_opus", "seven_day_sonnet"}


def test_reflect_cost_quota_flag_surfaces_gate_state(tmp_path):
    """/reflect:cost surface: reflect_cost.py --quota renders the same view."""
    _seed_state(tmp_path, {"status": "allowed_warning",
                           "surpassedThreshold": 0.8, "isUsingOverage": False})
    env = dict(os.environ)
    env["ANTHROPIC_API_KEY"] = ""
    cp = subprocess.run(
        [sys.executable, str(COST), "--quota", "--state-dir", str(tmp_path)],
        env=env, capture_output=True, text=True, timeout=30,
    )
    assert cp.returncode == 0, cp.stderr
    assert "quota gate: CLOSED" in cp.stdout
    for window in ("five_hour", "seven_day", "seven_day_opus", "seven_day_sonnet"):
        assert window in cp.stdout


# ── Defer marker + replay, and the zero-API-call guarantee ───────────────────

def test_closed_gate_defers_queue_without_calling_claude(tmp_path):
    """Acceptance: the gate never costs an API call, and a deferred queue is
    retained (not consumed) with the quota_near_limit marker written."""
    state = tmp_path / "state"
    _make_queue(state, n=2)
    _seed_state(state, {"status": "allowed_warning", "surpassedThreshold": 0.8,
                        "isUsingOverage": False})
    fake = _fake_claude(tmp_path, '{"type":"result","is_error":false}')
    _run_drain(state, REFLECT_DRAIN_CLAUDE_BIN=fake, REFLECT_DRAIN_DRY_RUN="0")
    # No API call was issued: the recorder never ran.
    assert not (tmp_path / "claude-calls.log").exists()
    # Queue retained in full — replayable once quota recovers.
    queue = (state / "pending_reflections.jsonl").read_text().strip()
    assert len(queue.splitlines()) == 2
    # Marker written with the bead's reason.
    marker = json.loads((state / "quota-deferred.json").read_text())
    assert marker["reason"] == "quota_near_limit"
    log = (state / "drain.log").read_text()
    assert "quota gate CLOSED" in log
    assert "quota_near_limit" in log


def test_deferred_queue_replays_once_quota_recovers(tmp_path):
    """Acceptance: the deferred-write marker is replayable — a recovered
    quota reopens the gate, the queue drains, and the marker clears."""
    state = tmp_path / "state"
    _make_queue(state, n=1)
    # 1) Quota near the wall: drain defers.
    _seed_state(state, {"status": "allowed_warning", "surpassedThreshold": 0.8,
                        "isUsingOverage": False})
    _run_drain(state, REFLECT_DRAIN_DRY_RUN="1")
    assert (state / "quota-deferred.json").exists()
    assert (state / "pending_reflections.jsonl").read_text().strip()
    # 2) Quota recovered (fresh allowed snapshot): the same queue replays.
    _seed_state(state, {"status": "allowed", "utilization": 0.1,
                        "isUsingOverage": False})
    _run_drain(state, REFLECT_DRAIN_DRY_RUN="1")
    assert (state / "pending_reflections.jsonl").read_text().strip() == ""
    # Gate-open check cleared the marker — its presence means "deferred now".
    assert not (state / "quota-deferred.json").exists()
    log = (state / "drain.log").read_text()
    assert "DRY_RUN=1" in log


def test_drain_ingests_telemetry_and_gates_next_entry(tmp_path):
    """End-to-end: entry 1's result envelope carries near-limit telemetry; the
    per-entry gate check closes before entry 2 (consult before EACH entry)."""
    state = tmp_path / "state"
    _make_queue(state, n=2)
    envelope = json.dumps({
        "type": "result", "is_error": False, "result": "captured", "num_turns": 1,
        "total_cost_usd": 0.01,
        "usage": {"input_tokens": 10, "output_tokens": 10},
        "rate_limit_info": {
            "status": "allowed_warning", "rateLimitType": "five_hour",
            "utilization": 0.97, "surpassedThreshold": 0.95,
            "isUsingOverage": False,
        },
    })
    fake = _fake_claude(tmp_path, envelope)
    _run_drain(state, REFLECT_DRAIN_CLAUDE_BIN=fake, REFLECT_DRAIN_DRY_RUN="0")
    # Exactly one claude call: entry 1 ran, entry 2 was quota-deferred.
    calls = (tmp_path / "claude-calls.log").read_text().strip().splitlines()
    assert len(calls) == 1
    # Telemetry persisted from the run's own output (no extra API calls).
    persisted = json.loads((state / "quota-state.json").read_text())
    assert persisted["five_hour"]["surpassedThreshold"] == 0.95
    # Entry 2 retained for replay; marker written.
    queue = (state / "pending_reflections.jsonl").read_text().strip()
    assert len(queue.splitlines()) == 1
    assert (state / "quota-deferred.json").exists()
    log = (state / "drain.log").read_text()
    assert "quota gate CLOSED" in log


def test_gate_disabled_env_skips_quota_entirely(tmp_path):
    state = tmp_path / "state"
    _make_queue(state, n=1)
    _seed_state(state, {"status": "rejected"})
    _run_drain(state, REFLECT_DRAIN_DRY_RUN="1", REFLECT_QUOTA_GATE="0")
    # Gate off: the entry processed despite the rejected snapshot.
    assert (state / "pending_reflections.jsonl").read_text().strip() == ""
    assert not (state / "quota-deferred.json").exists()


def test_check_reads_disk_only_and_clears_marker_when_open(tmp_path):
    """`check` works with no claude binary / no network and resolves a
    standing deferral when the gate is open."""
    quota_store.write_defer_marker(tmp_path, "quota_near_limit", "test")
    env = dict(os.environ)
    env["ANTHROPIC_API_KEY"] = ""
    env["PATH"] = "/usr/bin:/bin"  # no claude anywhere on this PATH
    cp = subprocess.run(
        [sys.executable, str(QUOTA), "check", "--state-dir", str(tmp_path)],
        env=env, capture_output=True, text=True, timeout=30,
    )
    verdict = json.loads(cp.stdout)
    assert verdict["abort"] is False
    assert not (tmp_path / "quota-deferred.json").exists()


def test_drain_syntax_still_valid():
    cp = subprocess.run(["bash", "-n", str(DRAIN)], capture_output=True, text=True)
    assert cp.returncode == 0, cp.stderr


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
