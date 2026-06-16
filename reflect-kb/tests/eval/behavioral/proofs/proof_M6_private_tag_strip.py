# ABOUTME: Behavioral proof for port M6 — <private> spans are stripped fail-closed at the
# ABOUTME: LLM-bound capture boundary (reflect_cascade slice), so private text can never reach
# ABOUTME: the drain model or become an indexed learning that recall could ever surface.
"""M6 private-tag strip proof (capture-boundary privacy primitive).

Port M6 is a CAPTURE/SIGNAL port, NOT a retrieval port. The supplied hypothesis
framed it as "a private span must not appear in recall OUTPUT" — but the real
diff (commit a4100886, "strip private tags at the LLM-prompt boundary") wires
``privacy_filter.strip_private`` into ``reflect_cascade.prepare`` at the
slice -> drain boundary, plus the mini-learning hook / todo_state /
test_outcome_parser. ``recall.py`` contains NO reference to privacy_filter:

    $ grep -niE 'private|privacy_filter' plugins/reflect/skills/recall/scripts/recall.py
    (no matches)

So the port's invariant is enforced strictly UPSTREAM of indexing: by the time
a learning could ever be retrieved, the ``<private>`` content was already
elided at write time. A recall-ranking assertion would therefore be vacuous for
this port — there is nothing private in the corpus to rank, because the capture
layer removed it. The strongest OBSERVABLE invariant lives where the behaviour
executes: the real ``reflect_cascade.prepare`` slice file, the exact bytes
handed to the drain LLM. This proof drives that real module (no mock, no stub).

INVARIANT (seeds + the import knob fully determine each outcome — no LLM runs
in the assertion; ``prepare`` is the no-LLM gate+slice step):

  1. STRIP + MARK (port ON): a transcript whose correction turn embeds
     ``<private>password hunter2 at prod-db.internal</private>`` produces a
     slice file in which neither ``hunter2`` nor ``prod-db.internal`` appears,
     while the visible ``[private content removed]`` marker DOES (silent
     elisions can change a correction's meaning) and the surrounding correction
     text ("never hardcode") survives. This is M6's whole reason to exist.

  2. FAIL-CLOSED (port ON): an UNCLOSED ``<private>`` tag (no closing tag)
     strips to end-of-text rather than leaking — the secret after the open tag
     never reaches the slice.

  3. KNOB FLIP / FALSIFIABLE (port OFF): the cascade imports the filter with a
     best-effort ``try: from privacy_filter import strip_private / except
     ImportError: pass``. We flip the port OFF by making that import raise
     ImportError (shadowing the module). With the port OFF the SAME seed leaks
     ``hunter2`` straight into the slice and emits NO marker. This proves the
     elision in (1) is caused by the M6 PORT, not by signal-slicing dropping the
     line for unrelated reasons. The knob is the privacy_filter import itself.

Falsifiability: if M6 were absent (filter not wired, or stripping broken),
assertion 1 would FAIL (hunter2 present / marker absent) and assertion 3 would
show NO difference between ON and OFF. If fail-closed were dropped (only closed
pairs stripped), assertion 2 would FAIL.

Surface used: capture (real reflect_cascade.prepare module), not the
behavioral_kb retrieval fixture — see above for why recall is the wrong surface
for this port. No torch model is loaded; this proof is fast.

PORT: M6
"""
from __future__ import annotations

import builtins
import importlib
import json
import os
import sys
from pathlib import Path

import pytest

# The reflect plugin scripts live alongside reflect-kb/. Resolve them the same
# way the SG1 / S7 capture-layer proofs do so this runs from either layout.
_CONFTEST_DIR = Path(__file__).resolve().parents[1]  # reflect-kb/tests/eval/behavioral
_PLUGIN_CANDIDATES = [
    _CONFTEST_DIR.parents[3] / "plugins" / "reflect" / "scripts",
    _CONFTEST_DIR.parents[2].parent / "plugins" / "reflect" / "scripts",
]
_PLUGIN_SCRIPTS = next((p for p in _PLUGIN_CANDIDATES if p.exists()), _PLUGIN_CANDIDATES[0])
if str(_PLUGIN_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_PLUGIN_SCRIPTS))

PRIVATE_MARKER = "[private content removed]"


def _write_transcript(path: Path, turns):
    """Write a Claude-style jsonl transcript the real reflect_gate parses."""
    with open(path, "w") as fh:
        for role, text in turns:
            fh.write(json.dumps({"message": {"role": role, "content": text}}) + "\n")
    return path


def _fresh_cascade(db_path: Path):
    """Reload reflect_cascade with REFLECT_DB_PATH pointed at a tmp sandbox so the
    cascade's dedup / chunk-hash bookkeeping never touches the developer's
    ~/.reflect DB. Returns the live module."""
    os.environ["REFLECT_DB_PATH"] = str(db_path)
    import reflect_config
    importlib.reload(reflect_config)
    import reflect_cascade
    importlib.reload(reflect_cascade)
    # Make sure the real privacy_filter is importable (port ON baseline).
    import privacy_filter  # noqa: F401
    importlib.reload(privacy_filter)
    return reflect_cascade


# Seed shared by the ON and OFF arms so the ONLY variable is the M6 knob.
_LEAKY_TURNS = [
    ("user", "set up the db connection"),
    ("assistant", "using the connection string from the env"),
    (
        "user",
        "no, never hardcode it — root cause was the env "
        "<private>password hunter2 at prod-db.internal</private> leaking "
        "into the committed config",
    ),
]


def test_M6_private_span_stripped_and_marked_at_cascade_slice(tmp_path):
    """Port ON: <private> content is elided + marked in the real slice; the
    correction survives."""
    cascade = _fresh_cascade(tmp_path / "reflect.db")
    transcript = _write_transcript(tmp_path / "t.jsonl", _LEAKY_TURNS)

    prep = cascade.prepare(transcript, out_path=str(tmp_path / "slice.txt"))
    assert prep.action == "reflect", (
        f"seed must trip the correction gate so a slice is written; got "
        f"action={prep.action!r} reason={prep.reason!r}"
    )
    slice_text = Path(prep.slice_path).read_text()

    # (1) STRIP: the private secret never reaches the LLM-bound slice.
    assert "hunter2" not in slice_text, (
        "M6 must strip <private> content before the slice reaches the drain "
        "model; the password 'hunter2' leaked into the slice file"
    )
    assert "prod-db.internal" not in slice_text, (
        "the private host 'prod-db.internal' leaked into the slice"
    )
    # (1) MARK: the elision is visible, not silent.
    assert PRIVATE_MARKER in slice_text, (
        "M6 must replace a <private> span with a visible marker so a downstream "
        "reader knows an elision happened; marker missing"
    )
    # (1) The correction itself survives — only the private span is elided.
    assert "never hardcode" in slice_text, (
        "the surrounding correction must survive — only the <private> span is "
        "removed, not the whole turn"
    )


def test_M6_unclosed_private_fails_closed_in_slice(tmp_path):
    """Port ON, fail-closed: an UNCLOSED <private> tag strips to end-of-text."""
    cascade = _fresh_cascade(tmp_path / "reflect.db")
    turns = [
        ("user", "configure the deploy"),
        ("assistant", "reading the deploy target"),
        (
            "user",
            "no, that is wrong — never commit the token "
            "<private>secret token glory-be-leaks-to-eof",
        ),
    ]
    transcript = _write_transcript(tmp_path / "t2.jsonl", turns)
    prep = cascade.prepare(transcript, out_path=str(tmp_path / "slice2.txt"))
    assert prep.action == "reflect", prep.reason
    slice_text = Path(prep.slice_path).read_text()

    assert "glory-be-leaks-to-eof" not in slice_text, (
        "fail-closed: an unclosed <private> tag must strip to end-of-text "
        "rather than leak the trailing secret"
    )
    assert "secret token" not in slice_text, (
        "the unclosed private region must be fully removed"
    )


def test_M6_knob_off_leaks_private_content(tmp_path, monkeypatch):
    """Falsifiable knob flip: with the privacy_filter import forced to fail
    (the cascade's `except ImportError: pass` fail-open branch), the SAME seed
    LEAKS the private secret into the slice and emits NO marker.

    This isolates M6 as the cause of the elision in the ON arm — proving it is
    the PORT, not signal-slicing, that removes the private span.
    """
    cascade = _fresh_cascade(tmp_path / "reflect.db")
    transcript = _write_transcript(tmp_path / "t.jsonl", _LEAKY_TURNS)

    real_import = builtins.__import__

    def _block_privacy_filter(name, *args, **kwargs):
        if name == "privacy_filter":
            raise ImportError("M6 knob OFF: privacy_filter unavailable")
        return real_import(name, *args, **kwargs)

    # Drop any cached module so the in-function `from privacy_filter import ...`
    # re-imports and hits our block.
    monkeypatch.delitem(sys.modules, "privacy_filter", raising=False)
    monkeypatch.setattr(builtins, "__import__", _block_privacy_filter)

    prep = cascade.prepare(transcript, out_path=str(tmp_path / "slice_off.txt"))
    assert prep.action == "reflect", (
        "with M6 OFF the cascade must STILL produce a slice (the filter is "
        f"best-effort, never hard-fails); got action={prep.action!r}"
    )
    slice_off = Path(prep.slice_path).read_text()

    assert "hunter2" in slice_off, (
        "control: with the privacy_filter import disabled the private secret "
        "MUST leak into the slice — if it does not, the elision in the ON arm "
        "was caused by something other than the M6 port and the proof is vacuous"
    )
    assert PRIVATE_MARKER not in slice_off, (
        "with M6 OFF there is no filter to insert the marker; its presence "
        "would mean the port was still running"
    )


def test_M6_nested_private_spans_do_not_leak():
    """Regression: a NESTED <private> span must not leak the outer tail.

    A non-greedy ``.*?</private>`` closes at the FIRST close tag, so on
    ``<private>A <private>B</private> C</private>`` it strips only through the
    inner close and leaks ``C`` (and a dangling close tag) to the LLM. The
    depth-aware stripper must remove the whole balanced span. Disjoint spans of
    the same tag must each be removed while the public text BETWEEN them
    survives. Drives the real ``privacy_filter.strip_private`` — no LLM.
    """
    import privacy_filter as pf

    nested = pf.strip_private(
        "<private>API_KEY=sk-LEAK1 and <private>pw=hunter2</private> "
        "never-log-this</private>"
    )
    for secret in ("sk-LEAK1", "hunter2", "never-log-this"):
        assert secret not in nested, f"nested span leaked {secret!r}: {nested!r}"
    assert "</private>" not in nested, f"dangling close tag left: {nested!r}"

    deep = pf.strip_private("<private>a<private>b<private>SEKRIT</private>c</private>d</private>TAIL")
    assert "SEKRIT" not in deep and "TAIL" in deep, deep

    disjoint = pf.strip_private("<private>X</private> PUBLIC <private>Y</private>")
    assert "X" not in disjoint and "Y" not in disjoint
    assert "PUBLIC" in disjoint, f"disjoint over-stripped public text: {disjoint!r}"


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
