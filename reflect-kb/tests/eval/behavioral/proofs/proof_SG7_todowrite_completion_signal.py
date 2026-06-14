# ABOUTME: Behavioral proof for SG7 — TodoWrite completion as a capture SIGNAL. Driving the
# ABOUTME: real todo_state module (no recall, no torch, no LLM): a todo that transitions
# ABOUTME: pending->in_progress->completed emits exactly ONE Process learning attributing the
# ABOUTME: file events + in_progress duration; the same item arriving already-completed with no
# ABOUTME: prior baseline emits NOTHING. Seeds + the prior-status flip fully determine each outcome.
"""SG7: PostToolUse TodoWrite-completion capture signal.

Port SG7 is a SIGNAL/CAPTURE port — its behaviour lives in the PostToolUse hook
path (``plugins/reflect/hooks/posttooluse_minilearning.py`` ->
``plugins/reflect/scripts/todo_state.py``), NOT in recall.py. There is no
retrieval and no embedding model involved, so this proof drives the REAL
``todo_state`` module functions the hook calls (``observe_todowrite``,
``record_file_event``) against a hermetic on-disk state dir + learnings dir.

The TRUE invariant (read off the real diff, commit be301020):

  A todo item produces a candidate "how I accomplished X" learning ONLY when it
  transitions TO ``status='completed'`` *from a PRIOR recorded non-completed
  status*. The detector keys off the per-session state diff:

      if item.status == 'completed'
         and old is not None              # there is a prior recorded entry
         and old.status != 'completed':   # and it wasn't already completed
              -> emit learning

  An item that arrives *already* completed on its first observation has no prior
  baseline to diff against (``old is None``) and is SKIPPED — observe_todowrite
  returns None and writes no learning file. That prior-status baseline IS the
  load-bearing knob: flipping it (present vs absent) flips emission on/off, so
  the signal is caused by the real diff logic and not by the text of the item.

DECISIVE knob ON vs OFF (the same item content in both arms):

  ARM (knob ON  — prior baseline present):
      observe(pending) -> observe(in_progress) -> record file event ->
      observe(completed)
    => observe_todowrite returns {"completed":[...]} for the LAST call,
       exactly ONE learning file is written, and that file's frontmatter is
       category: Process / confidence: medium / source: todo-completion, names
       the file touched while in_progress, and carries a duration_s.

  CONTROL (knob OFF — no prior baseline):
      observe(completed) as the very FIRST observation of the item
    => observe_todowrite returns None and ZERO learning files are written.

If SG7 were absent (no observe_todowrite call wired, or a broken diff that
emitted on any completed status), the CONTROL arm would ALSO emit a learning and
the test would FAIL. If SG7 never emitted, the ARM would produce no file and the
test would FAIL. No LLM participates: the seeds (todo lists) plus the
prior-status flip fully determine each assertion.

PORT: SG7
"""
from __future__ import annotations

import sys
import time
from pathlib import Path

import pytest

# todo_state lives in the reflect plugin scripts, alongside reflect-kb/. Resolve
# it the same way the SG1 capture-layer proof resolves reflect_db, so this runs
# from either checkout layout.
_BEHAVIORAL_DIR = Path(__file__).resolve().parents[1]  # reflect-kb/tests/eval/behavioral
_PLUGIN_CANDIDATES = [
    _BEHAVIORAL_DIR.parents[3] / "plugins" / "reflect" / "scripts",
    _BEHAVIORAL_DIR.parents[2].parent / "plugins" / "reflect" / "scripts",
]
_PLUGIN_SCRIPTS = next((p for p in _PLUGIN_CANDIDATES if p.exists()), _PLUGIN_CANDIDATES[0])
if str(_PLUGIN_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_PLUGIN_SCRIPTS))

import todo_state as ts  # noqa: E402
from todo_state import (  # noqa: E402
    observe_todowrite,
    record_file_event,
)


@pytest.fixture(autouse=True)
def _isolated_state(tmp_path, monkeypatch):
    """Point the real module at hermetic state + learnings dirs (same env knobs
    the hook honors), so this proof never touches the developer's ~/.reflect."""
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path / "state"))
    monkeypatch.setenv("REFLECT_LEARNINGS_DIR", str(tmp_path / "learnings"))
    yield tmp_path


def _todos(*items):
    # Mirror the real TodoWrite tool_input shape the hook receives.
    return {"todos": [{"content": c, "status": s, "activeForm": c} for c, s in items]}


def _learning_files(tmp_path: Path):
    d = tmp_path / "learnings"
    return sorted(d.glob("lrn-todo-done-*.md")) if d.is_dir() else []


def test_SG7_completion_with_prior_baseline_emits_process_learning(tmp_path):
    """ARM (knob ON): pending -> in_progress -> file event -> completed emits
    exactly one Process/medium/todo-completion learning attributing the file."""
    sid = "sess-sg7-arm"
    content = "Wire the SG7 TodoWrite completion signal"

    # The first two observations have no completion transition: a brand-new
    # pending item, then it goes in_progress. Each returns None and only
    # records baseline state.
    assert observe_todowrite(sid, _todos((content, "pending"))) is None
    assert observe_todowrite(sid, _todos((content, "in_progress"))) is None

    # A file is touched WHILE the item is in progress — this is the work the
    # completion will attribute.
    record_file_event(sid, "Edit", {"file_path": "/repo/plugins/reflect/scripts/todo_state.py"})

    # Small real gap so the in_progress duration is a positive, observable number.
    time.sleep(0.05)

    # The completing observation: prior recorded status is in_progress (!= completed),
    # so the real diff fires.
    hit = observe_todowrite(sid, _todos((content, "completed")))

    # 1. The signal fired: observe_todowrite returns the completion bundle.
    assert hit is not None, "completion with a prior in_progress baseline must fire the signal"
    assert len(hit["completed"]) == 1
    done = hit["completed"][0]
    assert done["content"] == content
    # 2. The file touched while in_progress is attributed to this completion.
    assert "/repo/plugins/reflect/scripts/todo_state.py" in done["files"]
    # 3. A positive in_progress duration was tracked.
    assert done["duration_s"] is not None and done["duration_s"] > 0
    # 4. A slug was returned, i.e. a learning file was actually written.
    assert done["learning"], "a learning slug must be returned"

    # 5. Exactly ONE learning file exists on disk with the SG7 frontmatter.
    files = _learning_files(tmp_path)
    assert len(files) == 1, f"expected exactly one learning file, got {[f.name for f in files]}"
    text = files[0].read_text(encoding="utf-8")
    assert "category: Process" in text
    assert "confidence: medium" in text
    assert "source: todo-completion" in text
    # the inferred execution detail is present in the body
    assert "/repo/plugins/reflect/scripts/todo_state.py" in text
    assert "duration_s:" in text


def test_SG7_already_completed_without_baseline_emits_nothing(tmp_path):
    """CONTROL (knob OFF): the SAME item arriving already-completed on its first
    observation has no prior baseline (old is None) and must emit NOTHING.

    This is the falsifiable half: it rules out the trivial 'emit a learning for
    any completed status' failure mode and proves the prior-status diff is what
    causes emission — same content as the ARM, only the baseline is removed."""
    sid = "sess-sg7-control"
    content = "Wire the SG7 TodoWrite completion signal"  # identical to the ARM

    # First and only observation: the item is ALREADY completed. No prior entry.
    hit = observe_todowrite(sid, _todos((content, "completed")))

    assert hit is None, "an already-completed item with no prior baseline must not fire the signal"
    assert _learning_files(tmp_path) == [], "no learning file may be written without a prior baseline"


def test_SG7_repeated_completed_list_is_idempotent(tmp_path):
    """A second TodoWrite that re-lists the item as completed must NOT emit a
    second learning: after the first transition the stored status is 'completed'
    so old.status == 'completed' and the diff no-ops. Pins that the signal is
    edge-triggered (the transition), not level-triggered (the status)."""
    sid = "sess-sg7-idem"
    content = "Edge-triggered completion"

    assert observe_todowrite(sid, _todos((content, "in_progress"))) is None
    first = observe_todowrite(sid, _todos((content, "completed")))
    assert first is not None and len(first["completed"]) == 1

    # Re-list the same completed item — no NEW transition.
    second = observe_todowrite(sid, _todos((content, "completed")))
    assert second is None, "re-listing an already-completed item must not re-fire"

    # Still exactly one learning file on disk.
    assert len(_learning_files(tmp_path)) == 1
