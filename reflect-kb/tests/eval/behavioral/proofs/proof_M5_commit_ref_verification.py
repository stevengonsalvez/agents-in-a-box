# ABOUTME: Behavioral proof for port M5 — commit_verifier extracts hex commit refs from a
# ABOUTME: learning body and verifies each against the real local repo via git cat-file, so a
# ABOUTME: fabricated sha is flagged unverified while a genuine repo sha is confirmed verified.
"""M5 commit-reference verification proof (capture/write-time integrity primitive).

Port M5 is a CAPTURE port, NOT a retrieval port. The real diff (commit
8c77718a, "verify commit refs in learnings before persistence") adds
``plugins/reflect/scripts/commit_verifier.py`` and wires it into
``output_generator.create_knowledge_note`` so that, BEFORE a learning is
persisted, every cited commit hash is checked against the project repo. A note
where EVERY cited ref is fabricated is rejected; surviving notes record
``unverified_refs`` in frontmatter so recall can downrank/warn. ``recall.py``
contains no reference to commit_verifier — the behaviour executes strictly
upstream of indexing, so the strongest OBSERVABLE invariant lives in the real
module, driven directly (no mock, no stub, no LLM).

The supplied hypothesis was correct in shape; this proof pins it against the
real code: extraction requires at least one DIGIT in the candidate (filtering
hex-shaped English words), and verification uses ``git cat-file -e
<sha>^{commit}`` in ``repo_dir``.

INVARIANT (seeds + the repo_dir knob fully determine each outcome — no LLM runs
in the assertion; git's object database is the oracle):

  1. EXTRACT: a body citing a real 8-hex repo sha (digits present) and a
     fabricated 12-hex sha (digits present) yields BOTH as candidate refs,
     while a hex-shaped English word with no digit ("deadbeef" -> filtered by
     the digit rule) is NOT extracted. Pure extraction, deterministic regex.

  2. VERIFY (port ON, repo_dir = the real reflect repo): ``verify_refs`` marks
     the genuine repo sha ``verified`` and the fabricated sha ``unverified``;
     the report is ``checked=True``. This is M5's whole reason to exist —
     git cat-file distinguishes the hallucination from the real citation.

  3. KNOB FLIP / FALSIFIABLE (port OFF, repo_dir = a non-repo tmp dir): the
     SAME body produces ``checked=False`` and flags NOTHING — the real sha is
     no longer "verified" and the bogus sha is no longer "unverified". This
     proves the verdict in (2) is caused by the git-backed verification against
     a real repo, not by text luck: absence of a repo means absence of
     evidence, and M5 refuses to call anything a hallucination without it.

  4. all_unverified gate: a body citing ONLY fabricated shas (the
     reject-the-note condition the writer keys on) reports ``all_unverified``
     True against the real repo, and False once a genuine sha is added — the
     exact boolean ``create_knowledge_note`` uses to reject a fully-hallucinated
     note.

Falsifiability: if verification were broken (always-verify), assertion 2 would
FAIL (bogus sha reported verified) and assertion 4 would FAIL. If the digit
filter were dropped, assertion 1 would FAIL ("deadbeef" extracted). If the
non-repo guard were dropped, assertion 3 would show the same verdict ON and OFF
and the proof would be vacuous.

Surface used: capture (real commit_verifier module), not the behavioral_kb
retrieval fixture — see above for why recall is the wrong surface for this port.
No torch model is loaded; this proof is fast.

PORT: M5
"""
from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

# The reflect plugin scripts live alongside reflect-kb/. Resolve them the same
# way the M6 capture-layer proof does so this runs from either layout.
_CONFTEST_DIR = Path(__file__).resolve().parents[1]  # reflect-kb/tests/eval/behavioral
_PLUGIN_CANDIDATES = [
    _CONFTEST_DIR.parents[3] / "plugins" / "reflect" / "scripts",
    _CONFTEST_DIR.parents[2].parent / "plugins" / "reflect" / "scripts",
]
_PLUGIN_SCRIPTS = next((p for p in _PLUGIN_CANDIDATES if p.exists()), _PLUGIN_CANDIDATES[0])
if str(_PLUGIN_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_PLUGIN_SCRIPTS))

import commit_verifier  # noqa: E402


def _repo_root() -> Path:
    """The real reflect git repo that contains commit_verifier.py — its object
    DB is the oracle for the genuine sha. Resolved from the module's own path so
    the proof never assumes the cwd is the repo."""
    root = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        cwd=_PLUGIN_SCRIPTS, capture_output=True, text=True, timeout=5,
    ).stdout.strip()
    assert root, "could not resolve the reflect repo root for the verification oracle"
    return Path(root)


# A GENUINE commit in this repo: the M5 port commit itself (full 40-hex form, so
# this proof is robust to short-hash ambiguity as the repo grows). Has digits ->
# survives the extractor's digit rule.
_REAL_SHA = "8c77718ae1342dd1a94685efc95c5d8733876f6c"
# A FABRICATED sha — valid hex, contains digits (so it is extracted), but no such
# object exists in the repo. The classic LLM hallucination shape.
_BOGUS_SHA = "abc1234def567"
# A hex-shaped English word with NO digit — must be filtered at extraction, never
# reaching git at all.
_NO_DIGIT_HEXWORD = "deadbeef"


def test_M5_real_sha_in_repo_is_a_real_object():
    """Guard: the genuine sha used as the oracle truly resolves in this repo, so
    a later 'verified' verdict is meaningful and not a false positive on a sha
    that happens not to exist."""
    root = _repo_root()
    r = subprocess.run(
        ["git", "cat-file", "-e", f"{_REAL_SHA}^{{commit}}"],
        cwd=root, capture_output=True, timeout=5,
    )
    assert r.returncode == 0, (
        "test oracle invalid: the supposedly-real sha does not resolve in this "
        "repo — pin a sha that exists before asserting verification"
    )


def test_M5_extract_requires_digit_filters_hexwords():
    """(1) EXTRACT: both digit-bearing shas are candidates; the no-digit
    hex-shaped English word is filtered out before any git call."""
    body = (
        f"Root cause fixed in {_REAL_SHA}; an earlier attempt {_BOGUS_SHA} "
        f"regressed it. (the word {_NO_DIGIT_HEXWORD} must not be treated as a sha)"
    )
    refs = commit_verifier.extract_commit_refs(body)

    assert _REAL_SHA in refs, "the genuine sha must be extracted as a candidate ref"
    assert _BOGUS_SHA in refs, "the fabricated sha must be extracted as a candidate ref"
    assert _NO_DIGIT_HEXWORD not in refs, (
        "a hex-shaped word with no digit ('deadbeef') must be filtered by the "
        "digit rule, never reaching git as a candidate sha"
    )


def test_M5_verify_marks_real_verified_and_bogus_unverified():
    """(2) VERIFY (port ON, real repo): git cat-file confirms the genuine sha
    and flags the fabricated one. This is the core M5 invariant — separating a
    real citation from an LLM hallucination."""
    body = f"Fixed in {_REAL_SHA}; superseded a broken {_BOGUS_SHA}."
    report = commit_verifier.verify_refs(body, repo_dir=_repo_root())

    assert report.checked is True, (
        "verification ran against a real git repo, so the report must be checked"
    )
    assert _REAL_SHA in report.verified, (
        "the genuine repo sha must land in `verified` — git cat-file resolves it"
    )
    assert _BOGUS_SHA in report.unverified, (
        "the fabricated sha must land in `unverified` — it has no object in the "
        "repo; this is the hallucination M5 exists to catch"
    )
    assert _BOGUS_SHA not in report.verified, "a fabricated sha must never be verified"
    assert _REAL_SHA not in report.unverified, "a genuine sha must never be unverified"


def test_M5_knob_off_non_repo_dir_flags_nothing(tmp_path):
    """(3) KNOB FLIP (port OFF, repo_dir = non-repo tmp dir): the SAME body
    yields checked=False and flags NOTHING — neither sha is verified nor
    unverified. Isolates the git-backed verification as the cause of the ON
    verdict: no repo => no evidence => no fabrication claim."""
    # tmp_path is a fresh dir with no .git — verify nothing can resolve here.
    assert not (tmp_path / ".git").exists()
    body = f"Fixed in {_REAL_SHA}; superseded a broken {_BOGUS_SHA}."
    report = commit_verifier.verify_refs(body, repo_dir=tmp_path)

    assert report.checked is False, (
        "outside a git repo M5 must report checked=False — absence of evidence "
        "is not fabrication evidence"
    )
    assert report.verified == [], (
        "with no repo to verify against, the genuine sha must NOT be reported "
        "verified — proving the ON verdict came from git, not from text"
    )
    assert report.unverified == [], (
        "with no repo, the fabricated sha must NOT be flagged unverified either "
        "— M5 refuses to call a sha a hallucination without a repo to check"
    )


def test_M5_all_unverified_gate_for_note_rejection():
    """(4) all_unverified gate: the boolean create_knowledge_note keys on to
    reject a fully-hallucinated note. Only-bogus body => True; add a genuine sha
    => False (the note now has at least one real citation and survives)."""
    only_bogus = f"Two attempts {_BOGUS_SHA} and 9f8e7d6c5b4a both failed."
    rep_bogus = commit_verifier.verify_refs(only_bogus, repo_dir=_repo_root())
    assert rep_bogus.all_unverified is True, (
        "a body citing ONLY fabricated shas must report all_unverified=True — "
        "this is the reject-the-note condition the writer depends on"
    )

    mixed = f"Real fix {_REAL_SHA} after a bogus {_BOGUS_SHA}."
    rep_mixed = commit_verifier.verify_refs(mixed, repo_dir=_repo_root())
    assert rep_mixed.all_unverified is False, (
        "once one genuine sha is present the note must NOT be wholly rejected — "
        "all_unverified must flip to False"
    )


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
