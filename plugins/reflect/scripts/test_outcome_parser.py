#!/usr/bin/env python3
# ABOUTME: Test-runner outcome parsing from Bash output (port SG4, agentmemory observe/classifier pattern).
# ABOUTME: Per-session failure-count state machine: N->0 writes a HIGH-confidence learning, 0->N arms a contradiction.
"""Test-outcome parser + per-session state machine.

Port SG4. Test outcomes are the highest-signal events in a coding session —
they're where "this works" is provable. This module parses Bash tool output
with a small set of test-runner regexes (pytest / jest / cargo / go) and
tracks a per-session, per-runner failure-count history at
``$REFLECT_STATE_DIR/test-state/<session_id>.json``.

Transitions (per runner, within a session):

* **fix**        — failures went N -> 0. The fix is *proven* by the test
                   runner, so we write a HIGH-confidence learning directly
                   (``source: test-outcome``) — no two-phase arming needed.
                   When the session had MULTIPLE failing runs before going
                   green, prior learnings captured for this session are
                   promoted one confidence tier and marked ``validated``.
* **regression** — failures went 0 -> N. Tests that used to pass now fail:
                   that's a contradiction signal. The PostToolUse hook arms
                   the mini-learning watcher with ``reason="test-regression"``
                   so the user's next corrective prompt is captured.

Everything here is stdlib-only and silent-fail shaped: any error returns
``None`` / no-ops rather than raising into the hook. State files expire after
``_STATE_TTL_S`` and are swept by :func:`cleanup_stale` (wired into the Stop
hook — the closest thing to a session-end hook the plugin has; the *live*
session's file is fresh and survives the sweep).
"""
from __future__ import annotations

import json
import os
import re
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

__all__ = [
    "TEST_RUNNERS",
    "TestOutcome",
    "parse_test_output",
    "record_outcome",
    "observe_bash",
    "write_fix_learning",
    "promote_session_learnings",
    "cleanup_session",
    "cleanup_stale",
]

_STATE_TTL_S = 6 * 3600   # stale session state expires after 6h (matches loop_detector)
_HISTORY_MAX = 20         # bounded per-runner run history
_PROMOTE_MIN_FAILING = 2  # "multiple failures" threshold for tier promotion
_TEXT_CAP = 200_000       # cap parsed output size (summary lines are near the end)

# Best-effort imports of shared scrub/strip helpers (same dir). A missing
# helper must never break parsing.
try:
    from silent_fail import scrub_secrets
except ImportError:  # pragma: no cover
    def scrub_secrets(text: str) -> str:  # type: ignore[no-redef]
        return text
try:
    from privacy_filter import strip_private
except ImportError:  # pragma: no cover
    def strip_private(text: str) -> str:  # type: ignore[no-redef]
        return text


# --- Runner regex set ------------------------------------------------------
#
# Each entry: detect regexes anchored on the runner's *summary* shape so
# arbitrary prose mentioning "passed" doesn't false-positive. Extraction is
# done per-runner in parse_test_output (counting rules differ).

TEST_RUNNERS: dict[str, dict[str, re.Pattern]] = {
    # pytest: "==== 3 failed, 10 passed in 1.23s ====" or, with -q,
    # the bare "3 failed, 10 passed in 1.23s" line (no = rails).
    "pytest": {
        "summary": re.compile(
            r"(?m)^(?:=+\s*)?"
            r"(?P<body>\d+\s+(?:passed|failed|errors?|xfailed|xpassed|skipped|warnings?)"
            r"[^=\n]*?\bin\s+[\d.]+s)"
            r"[^=\n]*?(?:=+)?\s*$"
        ),
    },
    # jest: "Tests:       1 failed, 2 passed, 3 total"
    "jest": {
        "summary": re.compile(r"(?m)^Tests:\s+(?P<body>.*\b\d+\s+total)\s*$"),
    },
    # cargo: "test result: ok. 10 passed; 0 failed; ..." (one line per binary)
    "cargo": {
        "summary": re.compile(
            r"(?m)^test result: (?:ok|FAILED)\.\s+(?P<passed>\d+)\s+passed;\s+(?P<failed>\d+)\s+failed"
        ),
    },
    # go test: "--- PASS: TestX" / "--- FAIL: TestX" (-v), or per-package
    # "ok  \tpkg\t0.5s" / "FAIL\tpkg\t0.1s" / bare trailing PASS / FAIL.
    "go": {
        "verbose": re.compile(r"(?m)^\s*--- (?P<status>PASS|FAIL): \S+"),
        "package_ok": re.compile(r"(?m)^ok\s+\S+\s+(?:[\d.]+s|\(cached\))"),
        "package_fail": re.compile(r"(?m)^FAIL\s+\S+(?:\s+[\d.]+s|\s+\[build failed\])?\s*$"),
        "bare": re.compile(r"(?m)^(?P<status>PASS|FAIL)\s*$"),
    },
}

_COUNT_FAILED = re.compile(r"(\d+)\s+(?:failed|errors?)\b")
_COUNT_PASSED = re.compile(r"(\d+)\s+passed\b")


@dataclass
class TestOutcome:
    """Parsed result of one test-runner invocation."""

    runner: str
    passed: int
    failed: int


def parse_test_output(text: str) -> Optional[TestOutcome]:
    """Parse test-runner output; return a TestOutcome or ``None``.

    Tries pytest, jest, cargo, go in order — first runner whose summary
    shape matches wins. Never raises.
    """
    if not text or not isinstance(text, str):
        return None
    try:
        text = text[-_TEXT_CAP:]

        # pytest / jest share the "N failed, M passed" body shape.
        for runner in ("pytest", "jest"):
            m = TEST_RUNNERS[runner]["summary"].search(text)
            if m:
                body = m.group("body")
                failed = sum(int(n) for n in _COUNT_FAILED.findall(body))
                passed = sum(int(n) for n in _COUNT_PASSED.findall(body))
                return TestOutcome(runner=runner, passed=passed, failed=failed)

        # cargo: sum across result lines (one per test binary).
        cargo_hits = list(TEST_RUNNERS["cargo"]["summary"].finditer(text))
        if cargo_hits:
            passed = sum(int(m.group("passed")) for m in cargo_hits)
            failed = sum(int(m.group("failed")) for m in cargo_hits)
            return TestOutcome(runner="cargo", passed=passed, failed=failed)

        # go: prefer -v per-test lines; fall back to package / bare lines.
        go = TEST_RUNNERS["go"]
        verbose = [m.group("status") for m in go["verbose"].finditer(text)]
        if verbose:
            return TestOutcome(
                runner="go",
                passed=sum(1 for s in verbose if s == "PASS"),
                failed=sum(1 for s in verbose if s == "FAIL"),
            )
        pkg_ok = len(go["package_ok"].findall(text))
        pkg_fail = len(go["package_fail"].findall(text))
        if pkg_ok or pkg_fail:
            return TestOutcome(runner="go", passed=pkg_ok, failed=pkg_fail)
        bare = [m.group("status") for m in go["bare"].finditer(text)]
        if bare:
            # Bare PASS/FAIL only — treat the run as one unit.
            return TestOutcome(
                runner="go",
                passed=1 if bare[-1] == "PASS" else 0,
                failed=1 if bare[-1] == "FAIL" else 0,
            )
    except Exception:  # noqa: BLE001 — hook-adjacent: never raise
        return None
    return None


# --- Per-session state machine ----------------------------------------------

def _state_dir() -> Path:
    base = Path(os.environ.get("REFLECT_STATE_DIR", Path.home() / ".reflect"))
    return base / "test-state"


def _state_path(session_id: str) -> Path:
    safe = "".join(c if c.isalnum() or c in "-_" else "_" for c in session_id)[:64]
    return _state_dir() / f"{safe}.json"


def _load(path: Path) -> dict:
    try:
        data = json.loads(path.read_text())
        if not isinstance(data, dict):
            return {}
        if time.time() - float(data.get("updated", 0)) > _STATE_TTL_S:
            return {}
        runners = data.get("runners", {})
        return runners if isinstance(runners, dict) else {}
    except (OSError, json.JSONDecodeError, ValueError):
        return {}


def _save(path: Path, runners: dict) -> None:
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp = path.with_suffix(".json.tmp")
        tmp.write_text(json.dumps({"updated": time.time(), "runners": runners}))
        tmp.replace(path)
    except OSError:
        pass


def record_outcome(session_id: str, outcome: TestOutcome) -> Optional[dict]:
    """Record one parsed outcome; return a transition dict or ``None``.

    Transition shapes:
      {"kind": "fix",        "prev_failed": N, "failing_runs": K, "promote": bool}
      {"kind": "regression", "prev_failed": 0, "new_failed": N}
    """
    if not session_id or outcome is None:
        return None
    try:
        path = _state_path(session_id)
        runners = _load(path)
        history = runners.get(outcome.runner, [])
        if not isinstance(history, list):
            history = []

        transition: Optional[dict] = None
        if history:
            prev_failed = int(history[-1].get("failed", 0))
            failing_runs = sum(1 for run in history if int(run.get("failed", 0)) > 0)
            if prev_failed > 0 and outcome.failed == 0:
                transition = {
                    "kind": "fix",
                    "prev_failed": prev_failed,
                    "failing_runs": failing_runs,
                    "promote": failing_runs >= _PROMOTE_MIN_FAILING,
                }
            elif prev_failed == 0 and outcome.failed > 0:
                transition = {
                    "kind": "regression",
                    "prev_failed": 0,
                    "new_failed": outcome.failed,
                }

        history.append(
            {"passed": outcome.passed, "failed": outcome.failed, "ts": time.time()}
        )
        runners[outcome.runner] = history[-_HISTORY_MAX:]
        _save(path, runners)
        return transition
    except Exception:  # noqa: BLE001
        return None


# --- Learning emission -------------------------------------------------------

def _learnings_dir() -> Path:
    """Where learnings get written. Honors REFLECT_LEARNINGS_DIR override;
    defaults to ~/.learnings/documents/ (same as the recall hooks)."""
    custom = os.environ.get("REFLECT_LEARNINGS_DIR")
    if custom:
        return Path(custom).expanduser()
    return Path.home() / ".learnings" / "documents"


def write_fix_learning(
    session_id: str,
    outcome: TestOutcome,
    transition: dict,
    command: str = "",
) -> Optional[str]:
    """Write the fix-confirmed HIGH-confidence learning; return its slug.

    The test runner *proved* the fix (failures N -> 0), which is why this
    skips the two-phase arm-then-correct flow and goes straight to disk
    with ``confidence: high`` / ``source: test-outcome``.
    """
    try:
        ld = _learnings_dir()
        ld.mkdir(parents=True, exist_ok=True)
        ts_ms = int(time.time() * 1000)
        slug = f"lrn-test-fix-{ts_ms}-{session_id[:8]}"
        path = ld / f"{slug}.md"
        n = 2
        while path.exists():
            path = ld / f"{slug}-{n}.md"
            n += 1
        cmd = scrub_secrets(strip_private(str(command))[:300])
        prev_failed = int(transition.get("prev_failed", 0))
        body = (
            f"---\n"
            f"id: {path.stem}\n"
            f"confidence: high\n"
            f"source: test-outcome\n"
            f"session_id: {session_id}\n"
            f"runner: {outcome.runner}\n"
            f"captured_at: {time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())}\n"
            f"---\n\n"
            f"# Test fix confirmed: {outcome.runner}\n\n"
            f"Failure count went **{prev_failed} -> 0** "
            f"({outcome.passed} passed) — the fix is proven by the test runner.\n\n"
            f"**Test command**: `{cmd}`\n\n"
            f"_Auto-captured by the PostToolUse test-outcome watcher. "
            f"Confidence is `high` because the transition was verified by "
            f"{outcome.runner} itself, not inferred from a correction._\n"
        )
        path.write_text(body, encoding="utf-8")
        return path.stem
    except Exception:  # noqa: BLE001
        return None


_CONF_LINE = re.compile(r"(?m)^confidence:\s*(\w+)\s*$")
_VALIDATED_LINE = re.compile(r"(?m)^validated:\s*true\s*$")
_TIER_UP = {"low": "medium", "medium": "high", "high": "high"}


def promote_session_learnings(session_id: str) -> int:
    """Mark this session's prior learnings ``validated: true`` and bump their
    confidence one tier (low -> medium -> high). Returns the number promoted.

    Fires on the all-pass-after-multiple-failures transition: a consistent
    green run is evidence the session's captured corrections were real.
    Skips ``source: test-outcome`` learnings (already high) and anything
    already validated. Never raises.
    """
    if not session_id:
        return 0
    promoted = 0
    try:
        ld = _learnings_dir()
        if not ld.is_dir():
            return 0
        marker = f"session_id: {session_id}"
        for path in ld.glob("*.md"):
            try:
                head = path.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            front = head[:1000]
            if marker not in front:
                continue
            if "source: test-outcome" in front:
                continue
            if _VALIDATED_LINE.search(front):
                continue
            m = _CONF_LINE.search(head)
            if not m:
                continue
            tier = m.group(1).lower()
            new_tier = _TIER_UP.get(tier, tier)
            replacement = f"confidence: {new_tier}\nvalidated: true"
            updated = head[: m.start()] + replacement + head[m.end():]
            try:
                path.write_text(updated, encoding="utf-8")
                promoted += 1
            except OSError:
                continue
    except Exception:  # noqa: BLE001
        return promoted
    return promoted


# --- Hook entry point ---------------------------------------------------------

def _response_text(tool_response) -> str:
    if isinstance(tool_response, str):
        return tool_response
    if isinstance(tool_response, dict):
        parts = []
        for key in ("stdout", "stderr", "output", "content"):
            v = tool_response.get(key)
            if isinstance(v, str) and v:
                parts.append(v)
        return "\n".join(parts)
    return ""


def observe_bash(session_id: str, tool_input, tool_response) -> Optional[dict]:
    """Parse one Bash tool_response; record state; act on transitions.

    Returns ``None`` when the output isn't test-runner output or nothing
    transitioned. Otherwise returns a dict the PostToolUse hook can act on:

      fix        -> {"kind": "fix", "runner", "passed", "failed",
                     "learning": <slug|None>, "promoted": <int>}
                    (learning already written here — hook does nothing more)
      regression -> {"kind": "regression", "runner", "passed", "failed",
                     "prev_failed": 0, "new_failed": N}
                    (hook arms the watcher with reason="test-regression")

    Silent-fail: never raises.
    """
    try:
        if not session_id:
            return None
        outcome = parse_test_output(_response_text(tool_response))
        if outcome is None:
            return None
        transition = record_outcome(session_id, outcome)
        if transition is None:
            return None

        command = ""
        if isinstance(tool_input, dict):
            command = str(tool_input.get("command", ""))
        elif tool_input:
            command = str(tool_input)

        result = {
            "kind": transition["kind"],
            "runner": outcome.runner,
            "passed": outcome.passed,
            "failed": outcome.failed,
        }
        if transition["kind"] == "fix":
            result["learning"] = write_fix_learning(
                session_id, outcome, transition, command
            )
            result["promoted"] = (
                promote_session_learnings(session_id)
                if transition.get("promote")
                else 0
            )
        else:  # regression
            result["prev_failed"] = transition.get("prev_failed", 0)
            result["new_failed"] = transition.get("new_failed", outcome.failed)
        return result
    except Exception:  # noqa: BLE001
        return None


# --- Cleanup -------------------------------------------------------------------

def cleanup_session(session_id: str) -> None:
    """Remove one session's test state (and its tmp sibling). Never raises."""
    if not session_id:
        return
    try:
        path = _state_path(session_id)
        for p in (path, path.with_suffix(".json.tmp")):
            try:
                p.unlink()
            except FileNotFoundError:
                pass
    except OSError:
        pass


def cleanup_stale(max_age_s: float = _STATE_TTL_S) -> int:
    """Sweep TTL-expired test-state files; return how many were removed.

    Wired into the Stop hook (the plugin has no SessionEnd event): the live
    session's state file is fresh and survives; only abandoned sessions'
    files get reaped. Never raises.
    """
    removed = 0
    try:
        d = _state_dir()
        if not d.is_dir():
            return 0
        now = time.time()
        for p in list(d.glob("*.json")) + list(d.glob("*.json.tmp")):
            try:
                if now - p.stat().st_mtime > max_age_s:
                    p.unlink()
                    removed += 1
            except OSError:
                continue
    except Exception:  # noqa: BLE001
        return removed
    return removed
