"""Tests for the silent-fail invariant on reflect hooks.

The recall + reflect hooks MUST NOT surface tracebacks into the user's
session even when downstream dependencies blow up (graphrag down, broken
cwd, missing libs). On any uncaught exception they should:

  * exit 0
  * emit valid empty JSON on stdout (or nothing for precompact)
  * leave a breadcrumb at ~/.reflect/last-event.json so the harness's
    status line can render ⚠ recall failed / ⚠ reflect failed

These tests force a raise by injecting a monkey-patched function that
throws, then assert the three invariants above.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import textwrap
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
RECALL_HOOK = PLUGIN_ROOT / "skills" / "recall" / "hooks" / "session_start_recall.py"
PRECOMPACT_HOOK = PLUGIN_ROOT / "hooks" / "precompact_reflect.py"


# Tests run the hook *script* in a subprocess so we exercise the same code
# path the harness does — including the top-level ``main()`` guard. Each
# subprocess is launched with REFLECT_STATE_DIR pointing at a tmp_path so
# the last-event.json breadcrumb lands in an isolated dir.


def _run_hook_forcing_raise(
    hook_path: Path,
    state_dir: Path,
    raise_in: str,
    stdin_payload: str = "{}",
) -> subprocess.CompletedProcess[str]:
    """Run ``hook_path`` after monkey-patching ``raise_in`` to throw.

    Gotcha: ``runpy.run_path`` returns a dict that is NOT the same dict
    used as the loaded functions' ``__globals__`` — so patching
    ``mod_globals[name]`` has no effect on subsequent function calls
    inside the module. We patch via ``main.__globals__`` instead, which
    IS the actual lookup dict for every function defined in the script.
    """
    shim = textwrap.dedent(f"""
        import os, runpy, sys
        os.environ["REFLECT_STATE_DIR"] = {str(state_dir)!r}

        target = {str(hook_path)!r}
        mod_globals = runpy.run_path(target, run_name="__pre_main__")

        # Patch the function's REAL globals (the dict its name lookups go
        # through), not the dict runpy hands back.
        real_globals = mod_globals["main"].__globals__

        def _boom(*a, **kw):
            raise RuntimeError("forced failure for silent-fail test")
        real_globals[{raise_in!r}] = _boom

        mod_globals["main"]()
    """)
    return subprocess.run(
        [sys.executable, "-c", shim],
        input=stdin_payload,
        capture_output=True,
        text=True,
        env={**os.environ, "REFLECT_STATE_DIR": str(state_dir)},
        timeout=20,
    )


# --- session_start_recall ----------------------------------------------------


def test_recall_silent_on_build_query_raise(tmp_path):
    """Force build_query() to raise → hook must exit 0, emit empty
    additionalContext, write last-event.json with event=error."""
    result = _run_hook_forcing_raise(
        RECALL_HOOK, tmp_path, raise_in="build_query"
    )
    assert result.returncode == 0, (
        f"recall hook exited non-zero on raised exception:\n"
        f"stdout={result.stdout!r}\nstderr={result.stderr!r}"
    )
    # Output must be parseable JSON with empty additionalContext.
    # (Some shim noise can land on stderr from runpy itself; we don't
    # police stderr in this test — we police what reaches the harness
    # via stdout, which is what gets fed back as hookSpecificOutput.)
    body = (result.stdout or "").strip()
    if body:
        parsed = json.loads(body)
        assert parsed["hookSpecificOutput"]["hookEventName"] == "SessionStart"
        assert parsed["hookSpecificOutput"]["additionalContext"] == ""

    # Breadcrumb written.
    breadcrumb = tmp_path / "last-event.json"
    assert breadcrumb.exists(), (
        f"expected breadcrumb at {breadcrumb}; tmp_path contents: "
        f"{list(tmp_path.iterdir())}"
    )
    event = json.loads(breadcrumb.read_text())
    assert event["event"] == "error"
    assert event["hook"] == "session_start_recall"
    assert event["kind"] == "RuntimeError"
    assert "forced failure" in event["detail"]
    assert isinstance(event["ts"], (int, float))


def test_recall_no_breadcrumb_on_happy_path(tmp_path):
    """Happy path (cwd=$HOME → emit("")) must NOT write an error
    breadcrumb. Asserts the silent-fail wrapper isn't firing spuriously."""
    result = subprocess.run(
        [sys.executable, str(RECALL_HOOK)],
        input="{}",
        capture_output=True,
        text=True,
        env={
            **os.environ,
            "REFLECT_STATE_DIR": str(tmp_path),
            "HOME": str(tmp_path),       # cwd=$HOME branch
            "CLAUDE_PROJECT_DIR": str(tmp_path),
        },
        timeout=20,
    )
    assert result.returncode == 0
    assert not (tmp_path / "last-event.json").exists()


# --- precompact_reflect ------------------------------------------------------


def test_precompact_silent_on_runtime_raise(tmp_path):
    """Force log_precompact_event() to raise → hook must exit 0 and
    write last-event.json with event=error."""
    result = _run_hook_forcing_raise(
        PRECOMPACT_HOOK, tmp_path,
        raise_in="log_precompact_event",
        stdin_payload='{"trigger":"auto"}',
    )
    assert result.returncode == 0, (
        f"precompact hook exited non-zero on raised exception:\n"
        f"stdout={result.stdout!r}\nstderr={result.stderr!r}"
    )
    breadcrumb = tmp_path / "last-event.json"
    assert breadcrumb.exists()
    event = json.loads(breadcrumb.read_text())
    assert event["event"] == "error"
    assert event["hook"] == "precompact_reflect"
    assert event["kind"] == "RuntimeError"


def test_precompact_breadcrumb_is_atomic(tmp_path):
    """Run twice in quick succession; the breadcrumb file must always
    be a complete valid JSON object (no half-written reads). We can't
    easily race here in a unit test, but we can at least assert the
    file persists as valid JSON after multiple writes."""
    for _ in range(3):
        _run_hook_forcing_raise(
            PRECOMPACT_HOOK, tmp_path,
            raise_in="log_precompact_event",
            stdin_payload='{"trigger":"auto"}',
        )
        # Each write must leave behind a valid JSON object.
        json.loads((tmp_path / "last-event.json").read_text())


# --- defense in depth -------------------------------------------------------


def test_recall_survives_corrupt_stdin(tmp_path):
    """A malformed stdin payload (not JSON) must not crash the hook."""
    result = subprocess.run(
        [sys.executable, str(RECALL_HOOK)],
        input="this is not valid json at all{{{",
        capture_output=True,
        text=True,
        env={
            **os.environ,
            "REFLECT_STATE_DIR": str(tmp_path),
            "HOME": str(tmp_path),
            "CLAUDE_PROJECT_DIR": str(tmp_path),
        },
        timeout=20,
    )
    assert result.returncode == 0


def test_precompact_survives_corrupt_stdin(tmp_path):
    """precompact_reflect already catches JSONDecodeError explicitly,
    but we keep this test as a regression guard."""
    result = subprocess.run(
        [sys.executable, str(PRECOMPACT_HOOK), "--log-only"],
        input="not json",
        capture_output=True,
        text=True,
        env={**os.environ, "REFLECT_STATE_DIR": str(tmp_path)},
        timeout=20,
    )
    assert result.returncode == 0
