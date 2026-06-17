# ABOUTME: Behavioral proof for R11 — SessionStart short-circuits after the
# ABOUTME: single tier-1 (skills) lookup when that hit is fresh AND high-score,
# ABOUTME: skipping the lower-tier recall entirely; stale/low-score hits fall
# ABOUTME: through. The freshness + rerank-score thresholds are the only knobs.
"""R11 forced-grounding short-circuit proof.

Port R11 (surface=inject) ports Hindsight's forced-grounding short-circuit
(agent.py:305 ``_all_mental_models_are_usable_and_fresh`` + agent.py:993-1003).
It ships entirely inside ``plugins/reflect/skills/recall/hooks/session_start_
recall.py``: ``freshness_check`` + ``should_short_circuit`` decide, and
``session_start_recall_payload`` orchestrates — on a warm project the tier-1
(skills) hit is fresh and high-confidence, so SessionStart emits the skills-only
inject and NEVER spawns the lower-tier ``recall.py`` subprocess.

This is a plugin-side hook port: it does NOT change recall ranking or the
reflect-kb engine, so there is nothing for the behavioral_kb retrieval fixture
to rank. This proof therefore drives the REAL hook module directly (imported by
file path, not re-implemented) and NO LLM / no embedding model runs in any
assertion — the verdict is fully determined by a small skill dict plus the two
deterministic thresholds.

Invariants (each arm's seed + the real module fully determine the verdict):

  A. SHORT-CIRCUIT ON A WARM HIT (decisive). With the tier-1 probe returning a
     FRESH (recent age), HIGH-score skill, the REAL
     ``session_start_recall_payload`` returns the skills-only block AND the
     lower-tier recall is NEVER invoked — proven by a spy on
     ``run_lower_tier_recall`` whose call count stays 0. If the short-circuit
     were broken, the spy would fire and this assertion FAILS.

  B. FALL THROUGH ON A COLD HIT. With a STALE skill (age past the freshness
     window) the orchestrator runs the lower-tier recall (spy fires exactly
     once) and returns its payload — cold projects still get full recall.

  C. THE SCORE THRESHOLD IS THE KNOB (falsifiable). The SAME fresh skill flips
     short-circuit on/off purely as its rerank score crosses the threshold:
     score just ABOVE => short-circuit (no lower tier); score just BELOW =>
     fall through (lower tier runs). Only the score differs between the two
     calls, so the threshold — not luck — owns the decision.

  D. REAL TIER-1 PROBE PARSES A REAL RESULT (no LLM). Driving the REAL
     ``skills_tier_probe`` against a fake ``recall.py`` that emits the exact
     ``render_json`` envelope proves the probe really parses JSON, maps the
     confidence tier to a score, and derives age from ``archived_at`` — the
     dict the decision consumes is produced by shipped code, not the test.

Falsifiability: if the short-circuit didn't suppress the lower tier, arm A's
spy count would be 1 and FAIL. If freshness were ignored, arm B would
short-circuit and the spy would stay 0 and FAIL. If the score gate were a
no-op, arm C's two outcomes would be identical and FAIL.

PORT: R11
"""
from __future__ import annotations

import importlib.util
import json
import os
import stat
import sys
from pathlib import Path

import pytest

# Import the REAL shipped hook module by file path so we exercise the actual
# short-circuit decision + orchestration, not a copy. parents[1] is the
# behavioral dir; parents[4] (== behavioral.parents[3]) is the repo root where
# plugins/ sits alongside reflect-kb/. A fallback covers a reflect-kb-as-root
# checkout.
_BEHAVIORAL_DIR = Path(__file__).resolve().parents[1]
_HOOK_CANDIDATES = [
    _BEHAVIORAL_DIR.parents[3] / "plugins" / "reflect" / "skills" / "recall" / "hooks",
    _BEHAVIORAL_DIR.parents[2].parent / "plugins" / "reflect" / "skills" / "recall" / "hooks",
]
_HOOK_DIR = next((p for p in _HOOK_CANDIDATES if p.exists()), _HOOK_CANDIDATES[0])
_HOOK_FILE = _HOOK_DIR / "session_start_recall.py"

_spec = importlib.util.spec_from_file_location("ssr_r11", _HOOK_FILE)
SSR = importlib.util.module_from_spec(_spec)
assert _spec and _spec.loader
_spec.loader.exec_module(SSR)


# --- helpers -------------------------------------------------------------

def _skill(content="use os.replace not rename", score=1.0, age_days=2, is_stale=None):
    """A tier-1 skills-tier hit in the shape the decision functions consume."""
    return {
        "id": "L-skill",
        "content": content,
        "score": score,
        "age_days": age_days,
        "is_stale": is_stale,
    }


class _Spy:
    """Records every call to the lower-tier recall so we can assert it ran 0/1x."""

    def __init__(self, ret="## lower tier inject\n"):
        self.calls = 0
        self.ret = ret

    def __call__(self, query, tags, recall):
        self.calls += 1
        return self.ret


def _patch_probe(monkeypatch, skill):
    """Make the tier-1 probe deterministically return ``skill`` (the INPUT to the
    real decision under test — not a mock of the decision itself)."""
    monkeypatch.setattr(SSR, "skills_tier_probe", lambda query, tags, recall: skill)


def _patch_lower(monkeypatch):
    spy = _Spy()
    monkeypatch.setattr(SSR, "run_lower_tier_recall", spy)
    return spy


# --- arm A: warm hit short-circuits, lower tier never runs ----------------

def test_warm_hit_short_circuits_and_skips_lower_tier(monkeypatch):
    """Fresh + high-score skill => skills-only payload, lower tier NOT invoked."""
    skill = _skill(score=1.0, age_days=2)
    _patch_probe(monkeypatch, skill)
    spy = _patch_lower(monkeypatch)

    payload = SSR.session_start_recall_payload("warmproj branch", ["recall"], Path("/x/recall.py"))

    assert spy.calls == 0, "lower-tier recall ran despite a warm short-circuit"
    # Payload is the skills-only tier, not the lower-tier block.
    assert payload == SSR.render_skills_only(skill, "warmproj branch")
    assert "Prior skill relevant" in payload
    assert "use os.replace" in payload


# --- arm B: cold (stale) hit falls through to the lower tier --------------

def test_cold_hit_falls_through_to_lower_tier(monkeypatch):
    """A stale skill (age past the freshness window) does NOT short-circuit; the
    lower-tier recall runs exactly once and its payload is returned."""
    skill = _skill(score=1.0, age_days=SSR.SHORT_CIRCUIT_MAX_AGE_DAYS + 5)
    _patch_probe(monkeypatch, skill)
    spy = _patch_lower(monkeypatch)

    payload = SSR.session_start_recall_payload("coldproj", ["recall"], Path("/x/recall.py"))

    assert spy.calls == 1, "lower-tier recall should run for a stale hit"
    assert payload == spy.ret


def test_no_skill_hit_falls_through(monkeypatch):
    """No tier-1 hit at all => fall through to the lower tier."""
    _patch_probe(monkeypatch, None)
    spy = _patch_lower(monkeypatch)

    payload = SSR.session_start_recall_payload("emptyproj", [], Path("/x/recall.py"))

    assert spy.calls == 1
    assert payload == spy.ret


# --- arm C: the rerank-score threshold is the single knob -----------------

def test_score_threshold_flips_short_circuit(monkeypatch):
    """Same FRESH skill: score just above the threshold short-circuits (lower
    tier silent); score just below falls through (lower tier runs). Only the
    score differs — so the threshold owns the decision."""
    thr = SSR.SHORT_CIRCUIT_SCORE_THRESHOLD

    # Above threshold -> short-circuit, lower tier never runs.
    above = _skill(score=thr + 0.05, age_days=1)
    _patch_probe(monkeypatch, above)
    spy_hi = _patch_lower(monkeypatch)
    out_hi = SSR.session_start_recall_payload("proj", ["t"], Path("/x/recall.py"))
    assert spy_hi.calls == 0
    assert out_hi == SSR.render_skills_only(above, "proj")

    # Below threshold -> fall through, lower tier runs once.
    below = _skill(score=thr - 0.05, age_days=1)
    _patch_probe(monkeypatch, below)
    spy_lo = _patch_lower(monkeypatch)
    out_lo = SSR.session_start_recall_payload("proj", ["t"], Path("/x/recall.py"))
    assert spy_lo.calls == 1
    assert out_lo == spy_lo.ret

    # Decisive: identical skill except score => opposite short-circuit verdict.
    assert (spy_hi.calls, spy_lo.calls) == (0, 1)


def test_exactly_at_threshold_does_not_short_circuit():
    """The gate is STRICTLY greater-than: a score exactly at the threshold does
    NOT short-circuit (pins the boundary so the knob is unambiguous)."""
    thr = SSR.SHORT_CIRCUIT_SCORE_THRESHOLD
    assert SSR.should_short_circuit(_skill(score=thr, age_days=1)) is False
    assert SSR.should_short_circuit(_skill(score=thr + 1e-9, age_days=1)) is True


# --- freshness_check unit invariants (mirrors Hindsight's gate) -----------

def test_freshness_requires_explicit_recent_provenance():
    """Fresh only when content present, not explicitly stale, and age known and
    within the window. Unknown age => NOT fresh (cannot short-circuit)."""
    assert SSR.freshness_check(_skill(age_days=2)) is True
    assert SSR.freshness_check(_skill(age_days=2, is_stale=True)) is False
    assert SSR.freshness_check(_skill(content="   ", age_days=2)) is False
    assert SSR.freshness_check(_skill(age_days=None)) is False  # unknown provenance
    assert SSR.freshness_check(_skill(age_days=SSR.SHORT_CIRCUIT_MAX_AGE_DAYS + 1)) is False


# --- arm D: the REAL tier-1 probe parses a REAL recall.py result ----------

def _fake_recall_script(tmp_path: Path, envelope: dict) -> Path:
    """A stand-in recall.py that prints a fixed render_json-shaped envelope.
    Drives the REAL probe's JSON parsing without an embedding model or KB."""
    script = tmp_path / "recall.py"
    # Emit the envelope verbatim on stdout — the probe reads stdout as JSON.
    script.write_text(
        "#!/usr/bin/env python3\n"
        "import sys\n"
        f"sys.stdout.write({json.dumps(json.dumps(envelope))})\n"
    )
    script.chmod(script.stat().st_mode | stat.S_IEXEC)
    return script


def test_real_probe_parses_json_and_maps_score_and_age(monkeypatch, tmp_path):
    """Drive the REAL skills_tier_probe against a fake recall.py emitting the
    render_json envelope: it must surface the top hit with a mapped score
    (HIGH -> 1.0) and a derived age — and that dict short-circuits."""
    from datetime import datetime, timedelta

    recent = (datetime.now() - timedelta(days=3)).isoformat()
    envelope = {
        "query": "q",
        "mode": "naive",
        "count": 1,
        "results": [
            {
                "id": "L-real",
                "title": "Skill title",
                "key_insight": "use os.replace for atomic rename",
                "confidence": "HIGH",
                "tags": ["fs"],
                "how_to_apply": "",
                "archived_at": recent,
            }
        ],
    }
    fake = _fake_recall_script(tmp_path, envelope)
    # Run the fake script with the test interpreter (no uv needed).
    monkeypatch.setattr(SSR, "UV_BIN", sys.executable)

    # Shim subprocess.run so `UV_BIN run --quiet <script> ...` becomes
    # `python <script>` — exercising the probe's real parse path end-to-end.
    import subprocess as _sp
    real_run = _sp.run

    def fake_run(cmd, **kw):
        # cmd == [UV_BIN, "run", "--quiet", str(recall), query, ...]
        recall_path = cmd[3]
        return real_run([sys.executable, recall_path], **kw)

    monkeypatch.setattr(SSR.subprocess, "run", fake_run)

    skill = SSR.skills_tier_probe("q", ["fs"], fake)
    assert skill is not None
    assert skill["id"] == "L-real"
    assert skill["content"] == "use os.replace for atomic rename"
    assert skill["score"] == 1.0  # HIGH tier -> 1.0
    assert skill["age_days"] is not None and skill["age_days"] <= 5
    # And a real warm hit short-circuits.
    assert SSR.should_short_circuit(skill) is True
