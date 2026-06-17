# ABOUTME: Behavioral proof for port A1 — pinned editable memory slots (agent-curated
# ABOUTME: scratchpads). Drives the REAL reflect_db slot ops + the REAL SessionStart Tier-0
# ABOUTME: slot_tier_context inject over a real sqlite DB: a pinned slot persists, edits in
# ABOUTME: place (same (project_id,name) key, new body), and surfaces ahead of recall when
# ABOUTME: REFLECT_SLOTS is on — yet the identical DB stays silent knob-off, and a read-only
# ABOUTME: slot rejects the agent edit (no surfacing). No LLM, no torch model, no ranker.
"""Port A1: pinned editable memory slots → Tier-0 SessionStart inject.

INVARIANT (storage surface, decisive by the REFLECT_SLOTS knob):
  A default slot is a *pinned* editable scratchpad: it is persisted on DB init and an
  agent edit rewrites it IN PLACE — same primary key (project_id, name), new body, bumped
  last_edited_at, no new row. When REFLECT_SLOTS is truthy that slot's content surfaces in
  the real SessionStart Tier-0 inject (slot_tier_context) *regardless of any retrieval
  ranking* — slots are working memory prepended ahead of skills/recall, not a scored hit.
  Turn the knob OFF over the EXACT SAME persisted DB and the slot does NOT surface
  (slot_tier_context returns ""), proving the PORT (the slots tier behind REFLECT_SLOTS),
  not text relevance, caused the surfacing. A read-only slot is the control: the agent
  edit is REFUSED (ok=False, content unchanged) so it never surfaces — a non-pinned-style
  row gets no scratchpad treatment.

WHY NO LLM / NO TORCH: every assertion is over (a) sqlite rows written by the real
reflect_db slot ops and (b) the deterministic markdown string built by the real
slot_tier_context hook function. slot_tier_context renders slots directly from sqlite and
NEVER shells out to recall.py — no embedding model, no cross-encoder, no LLM participates.
The only thing that flips surfacing is the REFLECT_SLOTS env literal and the read_only
flag, both pinned here.

This is a *storage*-surface port, so per the harness contract we drive the real module
directly (reflect_db slot ops + the SessionStart hook's slot_tier_context) rather than the
file-KB recall harness; A1's recall coupling is exactly "the slots block is prepended to
the SessionStart inject", which we assert on the real hook function's output string.

PORT: A1
"""
from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

# Resolve the real reflect plugin the same way proof_A3 does: this file lives at
# reflect-kb/tests/eval/behavioral/proofs/, so parents[5] is the repo root where plugins/
# sits beside reflect-kb/; parents[4].parent covers a standalone reflect-kb checkout.
_HERE = Path(__file__).resolve()
_CANDIDATES = [
    _HERE.parents[5] / "plugins" / "reflect",
    _HERE.parents[4].parent / "plugins" / "reflect",
]
_PLUGIN = next((p for p in _CANDIDATES if (p / "scripts" / "reflect_db.py").exists()), _CANDIDATES[0])
if not (_PLUGIN / "scripts" / "reflect_db.py").exists():
    raise RuntimeError(f"reflect plugin not found; tried {[str(p) for p in _CANDIDATES]}")
_SCRIPTS = _PLUGIN / "scripts"
sys.path.insert(0, str(_SCRIPTS))

import reflect_db  # noqa: E402


def _load_session_hook():
    """Import the real SessionStart hook module by path (it's not a package).

    The hook itself prepends scripts/ to sys.path at import time, so its lazy
    `import reflect_db` resolves to the same module object we monkeypatch below.
    """
    hook_path = _PLUGIN / "skills" / "recall" / "hooks" / "session_start_recall.py"
    assert hook_path.exists(), f"SessionStart hook missing: {hook_path}"
    spec = importlib.util.spec_from_file_location("session_start_recall", hook_path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


# A pinned slot we will edit. `pending_items` is a project-scope default slot, editable.
PINNED_SLOT = "pending_items"
PINNED_BODY = "- finish the A1 slot proof before the incident bridge call"
PINNED_BODY_V2 = "- A1 slot proof DONE; now wire the read-only guard regression"

# A read-only control. No default slot ships read_only, so we flip one explicitly to model
# the "protected / non-editable" row that must NOT accept the agent scratchpad edit.
RO_SLOT = "self_notes"
RO_TEXT = "- this edit must be refused"

PROJECT = "a1-proof-project"


@pytest.fixture
def db(tmp_path, monkeypatch):
    """A fresh isolated real sqlite reflect DB wired as the module default connection.

    Per-test isolation (the A3/S4 discipline): every test gets its own tmp DB file and its
    own get_conn override, so no slot content leaks across arms.
    """
    db_file = tmp_path / "reflect.db"
    conn = reflect_db.init_db(db_file)
    monkeypatch.setattr(reflect_db, "get_conn", lambda path=None: conn)
    yield conn
    reflect_db.close_all()


def _seed_and_pin(conn) -> dict:
    """Seed the 8 default slots, then make the agent edit (pin content into) PINNED_SLOT.

    Returns the slot dict after the edit. Asserts the edit-in-place storage invariant.
    """
    created = reflect_db.ensure_default_slots(PROJECT, conn=conn)
    assert created == 8, "fresh DB must seed exactly 8 pinned default slots"

    # Pre-edit: the pinned slot exists and is empty (persisted, not yet curated).
    before = reflect_db.get_slot(PINNED_SLOT, project_id=PROJECT, conn=conn)
    assert before is not None, "pinned slot must be persisted on init"
    assert before["content"] == "", "default slot starts empty"

    # Agent edit #1 (append) — the scratchpad write.
    r1 = reflect_db.slot_append(PINNED_SLOT, PINNED_BODY, project_id=PROJECT, conn=conn)
    assert r1["ok"] is True, r1

    # Agent edit #2 (replace) — edit IN PLACE: same row, new body.
    r2 = reflect_db.slot_replace(PINNED_SLOT, PINNED_BODY_V2, project_id=PROJECT, conn=conn)
    assert r2["ok"] is True, r2

    after = reflect_db.get_slot(PINNED_SLOT, project_id=PROJECT, conn=conn)
    return after


# ── ARM 1 (knob ON): pinned slot persists, edits in place, and SURFACES ahead of recall ──
def test_pinned_slot_persists_edits_in_place_and_surfaces_knob_on(db, monkeypatch):
    after = _seed_and_pin(db)

    # Storage invariant: edit-in-place. The body is the v2 replace value, and there is
    # exactly ONE row for (project_id, name) — the edit overwrote, it did not append a row.
    assert after["content"] == PINNED_BODY_V2, "replace must overwrite the body in place"
    assert after["last_edited_at"] >= after["created_at"], "edit must bump last_edited_at"
    n_rows = db.execute(
        "SELECT COUNT(*) AS c FROM slots WHERE project_id = ? AND name = ?",
        (PROJECT, PINNED_SLOT),
    ).fetchone()["c"]
    assert n_rows == 1, "edit-in-place: a slot edit must not spawn a second row"

    # Surfacing invariant (knob ON): the real Tier-0 inject renders the pinned slot's
    # content. This is the recall-coupling — slots prepend the SessionStart context.
    hook = _load_session_hook()
    monkeypatch.setattr(reflect_db, "get_conn", lambda path=None: db)
    monkeypatch.setattr(reflect_db, "derive_slot_project_id", lambda cwd=None: PROJECT)
    monkeypatch.setenv("REFLECT_SLOTS", "1")

    block = hook.slot_tier_context(Path("/tmp/does-not-matter"))
    assert block, "knob ON: a non-empty pinned slot must surface in the Tier-0 inject"
    assert PINNED_BODY_V2 in block, "the surfaced block must carry the pinned slot body"
    assert PINNED_SLOT in block, "the surfaced block must name the slot"
    # It surfaces regardless of ranking: no query, no scoring ran — pure render.
    assert block.lstrip().startswith("## Memory slots"), "slots render as the Tier-0 header"


# ── ARM 2 (knob OFF): the EXACT SAME persisted+edited DB stays silent ────────────────────
def test_same_pinned_slot_does_not_surface_knob_off(db, monkeypatch):
    after = _seed_and_pin(db)
    # Same persisted state as ARM 1: the pinned edit IS in the DB.
    assert after["content"] == PINNED_BODY_V2

    hook = _load_session_hook()
    monkeypatch.setattr(reflect_db, "get_conn", lambda path=None: db)
    monkeypatch.setattr(reflect_db, "derive_slot_project_id", lambda cwd=None: PROJECT)
    monkeypatch.delenv("REFLECT_SLOTS", raising=False)  # knob OFF

    block = hook.slot_tier_context(Path("/tmp/does-not-matter"))
    assert block == "", (
        "knob OFF: the identical persisted pinned slot must NOT surface — proving the "
        "REFLECT_SLOTS port, not text relevance, caused the surfacing in ARM 1"
    )


# ── ARM 3 (control): a read-only slot REFUSES the agent edit and never surfaces ──────────
def test_read_only_slot_rejects_edit_and_does_not_surface(db, monkeypatch):
    reflect_db.ensure_default_slots(PROJECT, conn=db)

    # Flip the control slot read-only — the "protected / non-scratchpad" row.
    db.execute(
        "UPDATE slots SET read_only = 1 WHERE project_id = ? AND name = ?",
        (PROJECT, RO_SLOT),
    )
    db.commit()

    # Agent edits must be refused on a read-only slot (append AND replace).
    ra = reflect_db.slot_append(RO_SLOT, RO_TEXT, project_id=PROJECT, conn=db)
    assert ra["ok"] is False and "read-only" in ra["error"], ra
    rr = reflect_db.slot_replace(RO_SLOT, RO_TEXT, project_id=PROJECT, conn=db)
    assert rr["ok"] is False and "read-only" in rr["error"], rr

    # The refused edit left no content, so even knob-ON it cannot surface.
    ro_after = reflect_db.get_slot(RO_SLOT, project_id=PROJECT, conn=db)
    assert ro_after["content"] == "", "refused edits must not mutate a read-only slot"

    hook = _load_session_hook()
    monkeypatch.setattr(reflect_db, "get_conn", lambda path=None: db)
    monkeypatch.setattr(reflect_db, "derive_slot_project_id", lambda cwd=None: PROJECT)
    monkeypatch.setenv("REFLECT_SLOTS", "1")

    block = hook.slot_tier_context(Path("/tmp/does-not-matter"))
    # All other slots are empty too, so the whole Tier-0 block is empty: the read-only
    # control got no scratchpad treatment and contributes nothing to recall.
    assert RO_TEXT not in block, "a read-only slot must never surface agent-rejected text"
    assert block == "", "with only an empty/read-only slot, the Tier-0 inject stays empty"
