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


# --- shared silent_fail helper ----------------------------------------------

# Imported lazily to exercise the real module under test.
def _import_silent_fail():
    sf_path = PLUGIN_ROOT / "scripts"
    sys.path.insert(0, str(sf_path))
    if "silent_fail" in sys.modules:
        del sys.modules["silent_fail"]
    import silent_fail
    return silent_fail


def test_scrub_secrets_masks_bearer_token():
    sf = _import_silent_fail()
    text = "OSError: 401 Unauthorized — Authorization: Bearer abc123xyz789TOKEN"
    out = sf.scrub_secrets(text)
    assert "abc123xyz789TOKEN" not in out
    assert "***REDACTED***" in out
    # Prefix preserved so the operator still sees WHERE the credential came from
    assert "Authorization" in out


def test_scrub_secrets_masks_api_key_assignments():
    sf = _import_silent_fail()
    for raw in (
        'api_key="sk-proj-zZ1234567890ABCdefgh"',
        "token=ghp_abcdefghijklmnopqrstuvwxyz1234567890",
        "password = SuperSecretPassword123",
        "X-Api-Key: my_secret_api_key_value_999",
    ):
        out = sf.scrub_secrets(raw)
        assert "***REDACTED***" in out, f"failed to mask: {raw!r} → {out!r}"


def test_scrub_secrets_masks_provider_key_shapes():
    sf = _import_silent_fail()
    text = (
        "Failed with key sk-proj-1234567890abcdefghij in path "
        "and ghp_abc123def456ghi789jkl012mno345pqr678"
    )
    out = sf.scrub_secrets(text)
    assert "sk-proj-1234567890abcdefghij" not in out
    assert "ghp_abc123def456ghi789jkl012mno345pqr678" not in out
    assert out.count("***REDACTED***") == 2


def test_scrub_secrets_idempotent_on_clean_text():
    sf = _import_silent_fail()
    text = "OSError: file not found at /tmp/x.json"
    assert sf.scrub_secrets(text) == text


def test_write_last_event_supports_ok_event(tmp_path, monkeypatch):
    """The breadcrumb writer must also support event='ok' for the
    upcoming status-line counter view (not just 'error')."""
    sf = _import_silent_fail()
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path))
    sf.write_last_event(
        hook_name="user_prompt_submit_recall",
        event="ok",
        kind="recall",
        detail="injected 3 learnings",
    )
    payload = json.loads((tmp_path / "last-event.json").read_text())
    assert payload["event"] == "ok"
    assert payload["hook"] == "user_prompt_submit_recall"


def test_write_last_event_resolves_env_per_call(tmp_path, monkeypatch):
    """Regression: previously LAST_EVENT_PATH was bound at import time,
    so changing REFLECT_STATE_DIR mid-process had no effect. The shared
    helper must resolve the env every call."""
    sf = _import_silent_fail()
    first = tmp_path / "a"
    second = tmp_path / "b"

    monkeypatch.setenv("REFLECT_STATE_DIR", str(first))
    sf.write_last_event(hook_name="h", event="error", kind="X", detail="d1")
    assert (first / "last-event.json").exists()

    monkeypatch.setenv("REFLECT_STATE_DIR", str(second))
    sf.write_last_event(hook_name="h", event="error", kind="X", detail="d2")
    assert (second / "last-event.json").exists()


def test_write_last_event_scrubs_secrets_in_detail(tmp_path, monkeypatch):
    """Detail field gets scrubbed before persisting so we don't write
    tokens to disk even via the breadcrumb."""
    sf = _import_silent_fail()
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path))
    sf.write_last_event(
        hook_name="h",
        event="error",
        kind="RuntimeError",
        detail="Failed with Bearer abc123xyz789TOKEN_VALUE",
    )
    payload = json.loads((tmp_path / "last-event.json").read_text())
    assert "abc123xyz789TOKEN_VALUE" not in payload["detail"]
    assert "***REDACTED***" in payload["detail"]


def test_forensics_log_goes_to_reflect_state_dir_not_claude(tmp_path, monkeypatch):
    """The fix for the codex bug: forensics_log MUST write to
    $REFLECT_STATE_DIR/logs/, NOT ~/.claude/logs/. Otherwise codex-fired
    precompact hooks write logs into a Claude-flavoured directory."""
    sf = _import_silent_fail()
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path))
    sf.forensics_log("precompact_reflect", "session=abc trigger=auto mode=remind")

    expected = tmp_path / "logs" / "precompact_reflect.log"
    assert expected.exists(), f"expected log at {expected}"
    contents = expected.read_text()
    assert "trigger=auto" in contents


def test_forensics_log_scrubs_secrets(tmp_path, monkeypatch):
    sf = _import_silent_fail()
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path))
    sf.forensics_log("h", "Calling api with token=ghp_abcdef1234567890ABCDEFGHIJ")
    contents = (tmp_path / "logs" / "h.log").read_text()
    assert "ghp_abcdef" not in contents
    assert "***REDACTED***" in contents


def test_precompact_log_path_is_harness_neutral(tmp_path):
    """End-to-end: run precompact_reflect.py with --log-only and confirm
    the log lands under $REFLECT_STATE_DIR/logs/, NOT ~/.claude/logs/.
    This is the regression test for the codex bug Stevie flagged."""
    result = subprocess.run(
        [sys.executable, str(PRECOMPACT_HOOK), "--log-only"],
        input='{"trigger":"auto","session_id":"deadbeef"}',
        capture_output=True,
        text=True,
        env={**os.environ, "REFLECT_STATE_DIR": str(tmp_path)},
        timeout=20,
    )
    assert result.returncode == 0
    expected = tmp_path / "logs" / "precompact_reflect.log"
    assert expected.exists(), (
        f"expected log at {expected}, contents of {tmp_path}: "
        f"{list(tmp_path.rglob('*'))}"
    )
    contents = expected.read_text()
    assert "trigger=auto" in contents
    assert "session=deadbeef" in contents


def test_drain_reflections_silent_fail_on_corrupt_queue(tmp_path):
    """sessionstart_drain_reflections.py must also silent-fail. Feed it
    a queue file whose parent directory is unreadable (simulated via
    REFLECT_STATE_DIR pointing at a file, not a dir) → exit 0, breadcrumb."""
    # Pre-create a regular file at the state dir path so get_state_dir
    # returns a path that can't be used as a directory.
    blocker = tmp_path / "not-a-dir"
    blocker.write_text("blocker", encoding="utf-8")

    drain_hook = PLUGIN_ROOT / "hooks" / "sessionstart_drain_reflections.py"
    result = subprocess.run(
        [sys.executable, str(drain_hook)],
        input="{}",
        capture_output=True,
        text=True,
        env={**os.environ, "REFLECT_STATE_DIR": str(blocker)},
        timeout=20,
    )
    assert result.returncode == 0, (
        f"drain hook exited non-zero on broken state dir:\n"
        f"stdout={result.stdout!r}\nstderr={result.stderr!r}"
    )


def test_recall_via_uv_run_script_silent_fail(tmp_path):
    """The recall hook runs in production via ``uv run --script`` (per
    its shebang). Verify the silent-fail wrapper still catches via the
    real invocation path, not just bare python3."""
    uv = subprocess.run(["uv", "--version"], capture_output=True, text=True)
    if uv.returncode != 0:
        pytest.skip("uv not available")

    result = subprocess.run(
        ["uv", "run", "--script", str(RECALL_HOOK)],
        input="{}",
        capture_output=True,
        text=True,
        env={
            **os.environ,
            "REFLECT_STATE_DIR": str(tmp_path),
            "HOME": str(tmp_path),
            "CLAUDE_PROJECT_DIR": str(tmp_path),  # cwd=$HOME early-exit
        },
        timeout=30,
    )
    # Even via uv invocation path, hook exits 0.
    assert result.returncode == 0, (
        f"uv-run recall hook exited non-zero:\n"
        f"stdout={result.stdout!r}\nstderr={result.stderr!r}"
    )
