# ABOUTME: Behavioral proof for S9 — volatile ranking signals live ONLY in the
# ABOUTME: reflect.db learning_signals sidecar, never in note frontmatter. Per-query
# ABOUTME: recall bumps mutate the sidecar row while the on-disk markdown note (bytes
# ABOUTME: AND mtime) stays byte-identical, and the write-time strip guard drops every
# ABOUTME: volatile key while passing durable fields through unchanged.
"""S9 volatile-signals-out-of-frontmatter proof.

Port S9 is a STORAGE port (surface=storage). Corrected against the real diff at
c9787fd2 (`feat(reflect): move volatile ranking signals into reflect.db sidecar
(S9)`), the true invariant lives in the reflect plugin's ``reflect_db.py`` — NOT
in recall.py — so there is no recall ranking knob and NO torch engine / LLM in
the assertion. We drive the REAL ``reflect_db`` module and the REAL note writer
``output_generator.create_knowledge_note`` directly.

The port moves the churning ranking signals (importance, maturity, recall_count,
helpful_count, ignored_count, stale_count, last_recalled_at) out of git-tracked
note markdown and into a ``learning_signals`` sidecar table inside reflect.db.
After a note is written its bytes are immutable: per-query recall bumps land only
in the DB, so team-shared knowledge notes produce clean git diffs and never
merge-conflict on telemetry.

Invariant (each arm seeds its own fresh state; the seed + the real module fully
determine the verdict — nothing here is decided by an LLM):

  Arm 1 — PER-QUERY BUMPS NEVER TOUCH MARKDOWN (decisive, with control).
     A real knowledge note is written to disk via the production
     ``create_knowledge_note`` writer; its exact bytes and mtime are captured.
     The learning is registered in reflect.db, then the REAL per-query recall
     path ``add_recall_event`` is fired three times. After the bumps:
       (a) the note file's bytes are byte-identical to the pre-bump capture
           and its mtime is unchanged — markdown was never rewritten;
       (b) NO volatile key (importance/maturity/recall_count/...) ever appears
           in the note's top-level frontmatter;
       (c) the sidecar row DID move: learning_signals.recall_count == 3 and
           last_recalled_at is populated — i.e. the telemetry the markdown did
           NOT absorb is provably tracked in its separate store.
     Control (proves (a)/(b) are not vacuous): a DURABLE field — the note
     ``title`` — IS present in frontmatter and equals what we wrote.

  Arm 2 — WRITE-TIME STRIP GUARD IS DECISIVE (knob-style on/off).
     ``strip_volatile_signal_fields`` is the write-time guard note writers apply
     before persisting markdown. Fed a frontmatter dict carrying BOTH durable
     fields (title, category, confidence_num) AND every member of
     VOLATILE_SIGNAL_FIELDS, it returns a copy where:
       - every volatile key is DROPPED (the "off" side: these never reach
         markdown), AND
       - every durable key passes through with its value intact (the "on"
         side: semantic content is untouched).
     The control is the identity case: a frontmatter with ONLY durable fields is
     returned unchanged. Toggling whether a key is in VOLATILE_SIGNAL_FIELDS is
     exactly what flips whether it survives — proving the guard owns the drop,
     not some incidental schema loss.

  Arm 3 — MISSING SIDECAR ROW READS AS DEFAULTS, not a frontmatter fallback.
     ``get_learning_signals`` for a learning whose row was never bumped returns
     the ByteRover defaults (importance 50, maturity 'draft', zero counters) —
     the read path is the sidecar, so a never-recalled learning is indistinguish-
     able from a missing row and never reaches into note markdown for signals.

Falsifiability: if a per-query bump rewrote the note (pre-S9 frontmatter
counters), arm 1(a) would FAIL on changed bytes/mtime. If any volatile key
leaked into frontmatter, arm 1(b) FAILS. If the bump didn't reach the sidecar,
arm 1(c)'s recall_count==3 FAILS. If the strip guard kept a volatile key or
dropped a durable one, arm 2 FAILS. If the read path fell back to markdown
instead of DB defaults, arm 3 FAILS.

PORT: S9
"""
from __future__ import annotations

import re
import sys
import time
from pathlib import Path

import pytest

# The port's production code lives in the reflect plugin; import the REAL modules
# so we exercise the shipped sidecar, not a copy. Path resolution mirrors
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

import output_generator  # noqa: E402
import reflect_db  # noqa: E402


# --- helpers ---------------------------------------------------------------


def _frontmatter_keys(text: str) -> set[str]:
    """Top-level frontmatter keys (comments and nested keys excluded)."""
    assert text.startswith("---"), "note is missing its YAML frontmatter block"
    end = text.find("\n---", 3)
    assert end != -1, "note frontmatter block is not terminated"
    header = text[3:end]
    keys: set[str] = set()
    for line in header.splitlines():
        m = re.match(r"^([A-Za-z_][A-Za-z0-9_]*):", line)
        if m:
            keys.add(m.group(1))
    return keys


@pytest.fixture()
def fresh_db(tmp_path, monkeypatch):
    """Fresh isolated reflect.db per arm, wired as the module default conn.

    Each arm that uses this gets a brand-new DB — no cross-arm signal state
    bleeds between tests."""
    db_file = tmp_path / "reflect.db"
    connection = reflect_db.init_db(db_file)
    monkeypatch.setattr(reflect_db, "get_conn", lambda path=None: connection)
    yield connection
    reflect_db.close_all()


@pytest.fixture()
def project(tmp_path, monkeypatch):
    """Isolated non-git project dir so notes land under tmp_path and the
    note writer's repo-relative resolution stays inside the sandbox."""
    monkeypatch.setenv("CLAUDE_PROJECT_DIR", str(tmp_path))
    monkeypatch.chdir(tmp_path)
    return tmp_path


# =========================================================================
# Arm 1 — per-query recall bumps never touch the markdown note
# =========================================================================
def test_recall_bumps_land_in_sidecar_not_markdown(fresh_db, project):
    conn = fresh_db

    # SEED: write a REAL knowledge note via the production writer.
    title = "Guard concurrent map writes with a mutex"
    filepath, _stem = output_generator.create_knowledge_note(
        title=title,
        category="concurrency",
        tags=["go", "race"],
        symptoms=["fatal error: concurrent map writes"],
        root_cause="unsynchronized map access from multiple goroutines",
        key_insight="serialize map access behind a sync.Mutex",
        problem="map mutated from two goroutines crashes the process",
        solution="wrap reads and writes in a mutex guard",
        confidence="high",
    )
    assert filepath.exists(), "note writer did not persist the markdown file"

    # Capture the note's bytes AND mtime BEFORE any recall telemetry.
    note_bytes_before = filepath.read_bytes()
    mtime_before = filepath.stat().st_mtime_ns
    fm_keys = _frontmatter_keys(note_bytes_before.decode("utf-8"))

    # The volatile keys must NEVER be in the freshly-written frontmatter.
    leaked = fm_keys & reflect_db.VOLATILE_SIGNAL_FIELDS
    assert not leaked, (
        f"volatile ranking signals leaked into note frontmatter at write time: "
        f"{sorted(leaked)}"
    )
    # CONTROL: a durable field DID land in frontmatter (proves the absence
    # check above is not vacuous — the writer really does emit frontmatter).
    assert "title" in fm_keys, "durable field 'title' missing from frontmatter"
    assert f"title: {title}" in note_bytes_before.decode("utf-8")

    # Register the learning so add_recall_event has a row to bump, then fire
    # the REAL per-query recall path three times.
    lid = reflect_db.add_learning(title=title, category="concurrency", conn=conn)
    # mtime resolution guard: ensure any rewrite would produce a different
    # mtime_ns than the capture above.
    time.sleep(0.01)
    reflect_db.add_recall_event(lid, "concurrent map write crash", conn=conn)
    reflect_db.add_recall_event(
        lid, "go race on shared map", feedback="helpful", conn=conn
    )
    reflect_db.add_recall_event(lid, "sync.Mutex map guard", conn=conn)

    # ASSERT (a): the markdown note was NEVER rewritten by the bumps.
    note_bytes_after = filepath.read_bytes()
    mtime_after = filepath.stat().st_mtime_ns
    assert note_bytes_after == note_bytes_before, (
        "per-query recall bumps rewrote the markdown note — volatile signals "
        "must live in the sidecar, not the file"
    )
    assert mtime_after == mtime_before, (
        "note file mtime changed without a byte change — the file was touched "
        "by the recall path"
    )

    # ASSERT (b): still no volatile key in frontmatter after the bumps.
    fm_keys_after = _frontmatter_keys(note_bytes_after.decode("utf-8"))
    assert not (fm_keys_after & reflect_db.VOLATILE_SIGNAL_FIELDS)

    # ASSERT (c): the telemetry the markdown did NOT absorb IS tracked in the
    # separate sidecar store — three recalls, one of them 'helpful'.
    signals = reflect_db.get_learning_signals(lid, conn=conn)
    assert signals["recall_count"] == 3, (
        f"sidecar recall_count should be 3, got {signals['recall_count']}"
    )
    assert signals["helpful_count"] == 1, (
        f"sidecar helpful_count should be 1, got {signals['helpful_count']}"
    )
    assert signals["last_recalled_at"], "last_recalled_at not stamped in sidecar"


# =========================================================================
# Arm 2 — write-time strip guard drops volatile keys, keeps durable ones
# =========================================================================
def test_strip_volatile_signal_fields_is_decisive():
    durable = {
        "title": "Guard concurrent map writes",
        "category": "concurrency",
        "confidence_num": 0.9,
    }
    # Build a frontmatter carrying durable fields AND every volatile key.
    volatile = {key: 7 for key in reflect_db.VOLATILE_SIGNAL_FIELDS}
    mixed = {**durable, **volatile}

    stripped = reflect_db.strip_volatile_signal_fields(mixed)

    # "off" side: NO volatile key survives the guard.
    surviving_volatile = set(stripped) & reflect_db.VOLATILE_SIGNAL_FIELDS
    assert not surviving_volatile, (
        f"strip guard let volatile keys through: {sorted(surviving_volatile)}"
    )
    # "on" side: every durable field passes through with value intact.
    for key, value in durable.items():
        assert stripped.get(key) == value, (
            f"strip guard dropped or mangled durable field {key!r}"
        )

    # CONTROL / identity: a frontmatter with only durable fields is unchanged
    # (the guard removes nothing it shouldn't).
    assert reflect_db.strip_volatile_signal_fields(dict(durable)) == durable

    # Toggling membership is exactly what flips survival: the SAME key value
    # survives when it is NOT a volatile field name.
    not_volatile = {"importance_note": 7}  # name not in VOLATILE_SIGNAL_FIELDS
    assert reflect_db.strip_volatile_signal_fields(not_volatile) == not_volatile


# =========================================================================
# Arm 3 — missing sidecar row reads as ByteRover defaults
# =========================================================================
def test_missing_signals_row_returns_defaults(fresh_db):
    conn = fresh_db
    lid = reflect_db.add_learning(
        title="A learning that is never recalled", conn=conn
    )
    # add_learning seeds a default row; force a never-touched read path by
    # asking for a learning_id that has no bumps applied.
    signals = reflect_db.get_learning_signals(lid, conn=conn)
    assert signals["importance"] == reflect_db.DEFAULT_IMPORTANCE
    assert signals["maturity"] == reflect_db.DEFAULT_MATURITY
    assert signals["recall_count"] == 0
    assert signals["helpful_count"] == 0
    assert signals["ignored_count"] == 0
    assert signals["stale_count"] == 0

    # A learning_id with NO row at all also yields defaults (not a crash, not a
    # frontmatter fallback) — the read path is the sidecar.
    defaults = reflect_db.get_learning_signals("nonexistent-id", conn=conn)
    assert defaults["importance"] == reflect_db.DEFAULT_IMPORTANCE
    assert defaults["maturity"] == reflect_db.DEFAULT_MATURITY
    assert defaults["recall_count"] == 0
