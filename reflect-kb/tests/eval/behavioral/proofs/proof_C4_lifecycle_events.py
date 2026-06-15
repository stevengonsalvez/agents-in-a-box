# ABOUTME: Behavioral proof for C4 — reflect_events.emit() appends exactly one
# ABOUTME: correct JSONL line per lifecycle moment to $REFLECT_STATE_DIR/events.jsonl
# ABOUTME: and the configured shell hook fires on ITS event only (not on others),
# ABOUTME: and the real cascade call site emits consolidation.completed end-to-end.
"""C4 lifecycle-events emit + per-event shell-hook proof.

Port C4 is a CONSOLIDATION/capture port (surface=consolidation). It ships
``plugins/reflect/scripts/reflect_events.py`` — a Hindsight-style local webhook
fan-out. ``recall.py`` and the GraphRAG engine never reference it: the signal is
produced entirely at *capture* time, when reflect crosses a lifecycle moment and
appends an event. So there is nothing to rank and the behavioral_kb retrieval
fixture is the wrong surface. This proof drives the REAL module directly (no
mock of the thing under test, no torch engine, no network) and the cascade call
site through ``reflect_cascade.prepare`` — and NO LLM runs in any assertion:
``emit`` is a pure append + a deterministic ``subprocess.run`` of a shell hook.

The append target is ``$REFLECT_STATE_DIR/events.jsonl`` (resolved at call time),
so each test points the env at an isolated tmp dir. The write is append-safe (a
single ``os.write`` to an ``O_APPEND`` fd), so two emits never clobber a line.

Invariants (each arm's seed + the module fully determine the verdict — no LLM):

  A. ONE LINE PER MOMENT, CORRECT SHAPE. For EVERY one of the 4 lifecycle
     events (learning.created, learning.updated, skill.refreshed,
     consolidation.completed), a single ``emit(event, payload)`` appends EXACTLY
     one line, that line is valid JSON, its ``event`` equals the name emitted,
     and its ``payload`` round-trips the dict passed in. Two emits -> exactly two
     lines, in order. This pins "events appended per lifecycle moment".

  B. CLOSED VOCABULARY. An out-of-set event name (a typo a call site could
     plausibly introduce) is REJECTED with ``UnknownEvent`` and writes NOTHING —
     proving the gate is the closed enum, not incidental I/O.

  C. HOOK FIRES ON ITS EVENT, NOT OTHERS (decisive). With a shell hook
     configured for ``skill.refreshed`` ONLY (it ``touch``es a sentinel file),
     emitting ``learning.created`` leaves the sentinel ABSENT; emitting
     ``skill.refreshed`` CREATES it. Same module, same state dir — only the
     emitted event differs — so the hook selection is the port's doing, not luck.
     This is the falsifiable arm: if hooks fired indiscriminately, the sentinel
     would exist after arm-C's first emit and the assertion would FAIL.

  D. HOOK SEES THE EVENT NAME. The configured hook can branch on the firing
     event: it writes ``$REFLECT_EVENT`` into a capture file, and that file
     contains exactly the event name that fired (proving the env contract, not
     just a blind side effect).

  E. REAL CASCADE CALL SITE. Driving the REAL ``reflect_cascade.prepare`` on a
     signal-bearing transcript (deterministically reaching the reflect path)
     appends exactly one ``consolidation.completed`` line to the SAME
     events.jsonl — proving the additive call site wired into the shipped
     cascade actually emits, not just the unit API.

Falsifiability: if emit appended zero or two lines, arm A fails. If the enum
were open, arm B's emit would write a line and the "nothing written" assertion
would FAIL. If the hook fired on every event, arm C's negative case fails. If
the cascade call site were not wired, arm E finds no event line and FAILS.

PORT: C4
"""
from __future__ import annotations

import json
import os
import stat
import sys
import tempfile
from pathlib import Path

import pytest

# The events module lives in the reflect plugin; import the REAL module so we
# exercise the shipped emit + hook runner, not a copy. Path resolution mirrors
# proof_S2_typed_causal_link_enum.py: parents[3] is the repo root where plugins/
# sits alongside reflect-kb/; the fallback handles a reflect-kb-as-root checkout.
_BEHAVIORAL_DIR = Path(__file__).resolve().parents[1]
_PLUGIN_CANDIDATES = [
    _BEHAVIORAL_DIR.parents[3] / "plugins" / "reflect" / "scripts",
    _BEHAVIORAL_DIR.parents[2].parent / "plugins" / "reflect" / "scripts",
]
_PLUGIN_SCRIPTS = next(
    (p for p in _PLUGIN_CANDIDATES if p.exists()), _PLUGIN_CANDIDATES[0]
)
if str(_PLUGIN_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_PLUGIN_SCRIPTS))

import reflect_events as E  # noqa: E402


# --- helpers -------------------------------------------------------------

def _point_state_dir(monkeypatch, tmp_path: Path) -> Path:
    """Repoint REFLECT_STATE_DIR at an isolated tmp dir; return events.jsonl."""
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path))
    # Sanity: the module resolves the env at call time, so the path must follow.
    assert E.events_path() == tmp_path / "events.jsonl"
    return tmp_path / "events.jsonl"


def _read_lines(path: Path) -> list[dict]:
    if not path.exists():
        return []
    return [json.loads(ln) for ln in path.read_text().splitlines() if ln.strip()]


# --- arm A: exactly one correct JSONL line per lifecycle moment ----------

@pytest.mark.parametrize("event", E.EVENT_TYPES)
def test_emit_appends_one_correct_line_per_event(monkeypatch, tmp_path, event):
    """For each of the 4 lifecycle moments, emit appends exactly one valid JSON
    line whose event + payload match what was emitted."""
    ev_file = _point_state_dir(monkeypatch, tmp_path)
    payload = {"id": "L-" + event, "n": 7}

    assert E.emit(event, payload) is True

    lines = _read_lines(ev_file)
    assert len(lines) == 1, lines
    rec = lines[0]
    assert rec["event"] == event
    assert rec["payload"] == payload  # round-trips verbatim
    assert isinstance(rec["ts"], (int, float))


def test_two_emits_append_two_ordered_lines(monkeypatch, tmp_path):
    """Append-only: N emits -> N lines, in emit order, never clobbering."""
    ev_file = _point_state_dir(monkeypatch, tmp_path)

    assert E.emit("learning.created", {"id": "A"}) is True
    assert E.emit("learning.updated", {"id": "A", "rev": 2}) is True

    lines = _read_lines(ev_file)
    assert [r["event"] for r in lines] == ["learning.created", "learning.updated"]
    assert [r["payload"]["id"] for r in lines] == ["A", "A"]


# --- arm B: closed vocabulary — unknown event rejected, writes nothing ----

def test_unknown_event_rejected_and_writes_nothing(monkeypatch, tmp_path):
    """An out-of-enum event name a call site could typo is rejected with
    UnknownEvent and leaves events.jsonl absent — the gate is the closed set."""
    ev_file = _point_state_dir(monkeypatch, tmp_path)

    with pytest.raises(E.UnknownEvent):
        E.emit("learning.deleted")  # not in EVENT_TYPES

    assert not ev_file.exists()  # nothing was written


# --- arm C/D: shell hook fires on ITS event only, and sees the event name -

def _write_hook(tmp_path: Path, sentinel: Path, capture: Path) -> str:
    """A tiny shell hook that records the firing event + a sentinel touch."""
    script = tmp_path / "hook.sh"
    script.write_text(
        "#!/bin/sh\n"
        f'printf "%s" "$REFLECT_EVENT" > "{capture}"\n'
        f'touch "{sentinel}"\n'
    )
    script.chmod(script.stat().st_mode | stat.S_IEXEC)
    return f"sh {script}"


def test_configured_hook_fires_on_its_event_not_others(monkeypatch, tmp_path):
    """A hook wired to skill.refreshed ONLY: it stays silent on learning.created
    and fires (touches the sentinel) on skill.refreshed. The negative case is the
    decisive arm — indiscriminate firing would already touch the sentinel."""
    _point_state_dir(monkeypatch, tmp_path)
    sentinel = tmp_path / "sentinel"
    capture = tmp_path / "captured-event"

    monkeypatch.setenv(
        "REFLECT_EVENTS_ON_SKILL_REFRESHED", _write_hook(tmp_path, sentinel, capture)
    )

    # A different event must NOT fire the skill.refreshed hook.
    assert E.emit("learning.created", {"id": "x"}) is True
    assert not sentinel.exists(), "hook fired on the wrong event"

    # The configured event MUST fire the hook.
    assert E.emit("skill.refreshed", {"skill": "recall"}) is True
    assert sentinel.exists(), "hook did not fire on its own event"

    # arm D: the hook saw the actual event name via $REFLECT_EVENT.
    assert capture.read_text() == "skill.refreshed"


def test_hook_absent_emit_still_records(monkeypatch, tmp_path):
    """With NO hook configured, emit still records the line (the JSONL log is the
    primary contract; the shell hook is the optional side effect)."""
    ev_file = _point_state_dir(monkeypatch, tmp_path)
    monkeypatch.delenv("REFLECT_EVENTS_ON_SKILL_REFRESHED", raising=False)

    assert E.emit("skill.refreshed") is True
    lines = _read_lines(ev_file)
    assert len(lines) == 1 and lines[0]["event"] == "skill.refreshed"


# --- arm E: the REAL cascade call site emits consolidation.completed ------

def test_cascade_prepare_emits_consolidation_completed(monkeypatch, tmp_path):
    """Driving the REAL reflect_cascade.prepare on a signal-bearing transcript
    (deterministically reaching the reflect path) appends exactly one
    consolidation.completed line to the same events.jsonl — proving the additive
    cascade call site is wired, not just the unit API."""
    import reflect_cascade as C

    ev_file = _point_state_dir(monkeypatch, tmp_path)

    # A minimal transcript with a clear correction signal so the gate passes and
    # prepare reaches the reflect (slice-written) path deterministically.
    transcript = tmp_path / "t.jsonl"
    rows = [
        {"type": "user", "message": {"role": "user",
         "content": "no actually that is wrong, use os.replace instead of rename"}},
        {"type": "assistant", "message": {"role": "assistant",
         "content": [{"type": "text", "text": "You are right — fixing it now."}]}},
    ]
    transcript.write_text("\n".join(json.dumps(r) for r in rows) + "\n")

    fd, out = tempfile.mkstemp(suffix=".txt")
    os.close(fd)
    prep = C.prepare(transcript, out_path=out)
    assert prep.action == "reflect", prep  # deterministic: signal present

    lines = _read_lines(ev_file)
    consolidations = [r for r in lines if r["event"] == "consolidation.completed"]
    assert len(consolidations) == 1, lines
    assert consolidations[0]["payload"]["action"] == "reflect"
