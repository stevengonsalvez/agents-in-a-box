# ABOUTME: Behavioral proof for port M4 — mode_loader resolves a pluggable mode by overlaying a
# ABOUTME: child mode onto its single named parent (one level only), and REFLECT_MODE switches it.
"""M4 pluggable-mode inheritance proof (capture/signal port, NOT a retrieval port).

Port M4 lives in ``plugins/reflect/scripts/mode_loader.py`` (commit 7f985102),
a stdlib-only module that the reflect drain/signal layer reads its taxonomy and
prompt templates from. ``recall.py`` has NO reference to it — the mode is
resolved entirely at capture/drain time, so the behavioral_kb retrieval fixture
is the WRONG surface here (there is nothing to rank; the invariant is "what does
the resolved mode config contain"). This proof drives the REAL module directly
(no mock, no stub, no torch — fast). No LLM runs in any assertion: ``load_mode``
and ``deep_merge`` are pure, deterministic dict operations over JSON files.

The supplied hypothesis named ``recall.py`` / ``DEFAULT_MODE`` as the surface;
the real diff puts the surface in ``mode_loader.py`` (``REFLECT_MODE`` env,
``REFLECT_MODES_DIR`` env, ``DEFAULT_MODE="engineering"``, ``load_mode`` +
``deep_merge`` + ``parse_inheritance``). The invariant is corrected to the real
code:

  INHERITANCE is SINGLE LEVEL via ``parent--override`` file naming. Loading
  ``parent--override`` deep-merges ``override.json`` on top of ``parent.json``;
  the override replaces scalars/lists it declares and inherits everything it
  does not. ``a--b--c`` (two levels) is a hard error. Because the parent is
  loaded as a *simple* mode (it does not itself re-resolve a grandparent), a key
  that lives only in a grandparent file does NOT reach the leaf unless the
  direct parent re-declares it.

ARMS (each seeds its OWN fresh modes dir + clears the module mode cache, so no
state leaks between arms):

  1. INHERIT + OVERRIDE (port ON): ``engineering--zh`` inherits the parent's
     ``name`` / ``learning_types`` (keys it does NOT declare) AND overrides
     ``locale`` (the one key it declares: en -> zh). This is the shipped
     locale-variant shape: a one-key child file produces a full merged mode.

  2. EXACTLY ONE LEVEL (falsifiable guard): ``parse_inheritance("a--b--c")``
     raises ``ModeError`` — more than one ``--`` is rejected, matching the
     claude-mem single-level invariant. A bare ``parent--override`` (one ``--``)
     does not raise.

  3. GRANDPARENT NOT PULLED THROUGH (decisive single-level test): a key that
     exists ONLY in a grandparent file is ABSENT from a leaf whose direct parent
     does not re-declare it — then PRESENT once the direct parent re-declares it.
     This is the contrast that proves inheritance is one level, not transitive:
     if the loader chained grandparents, the grandparent-only key would leak in.

  4. REFLECT_MODE SWITCHES THE RESOLVED CONFIG (the env knob from the diff):
     with ``REFLECT_MODE=engineering`` the resolved locale is ``en`` and the
     auto-derived LANGUAGE REQUIREMENTS directive is empty; flipping
     ``REFLECT_MODE=engineering--zh`` resolves locale ``zh`` and a non-empty
     directive — same files, the env var alone changes the resolved config.
     This proves the PORT (env-driven resolution) caused the difference.

Falsifiability: if M4 were absent or merge were broken, (1) would drop the
inherited parent keys; if multi-level were allowed, (2) would not raise and (3)'s
grandparent key would leak through; if ``REFLECT_MODE`` were ignored, (4) would
return the same config for both values.

Surface used: signal (real mode_loader module), not the behavioral_kb retrieval
fixture — see above. No torch model is loaded; this proof is fast.

PORT: M4
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

# mode_loader lives in the reflect plugin scripts dir. Resolve it the same way
# the SG5 / M6 capture-layer proofs resolve their modules so this runs from the
# repo layout regardless of cwd.
_CONFTEST_DIR = Path(__file__).resolve().parents[1]  # reflect-kb/tests/eval/behavioral
_PLUGIN_CANDIDATES = [
    _CONFTEST_DIR.parents[3] / "plugins" / "reflect" / "scripts",
    _CONFTEST_DIR.parents[2].parent / "plugins" / "reflect" / "scripts",
]
_PLUGIN_SCRIPTS = next(
    (p for p in _PLUGIN_CANDIDATES if p.exists()), _PLUGIN_CANDIDATES[0]
)
if str(_PLUGIN_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_PLUGIN_SCRIPTS))

import mode_loader  # noqa: E402
from mode_loader import (  # noqa: E402
    DEFAULT_MODE,
    ModeError,
    deep_merge,
    get_active_mode,
    get_locale,
    language_requirement,
    load_mode,
    parse_inheritance,
    resolve_active_mode_id,
)


def _write_mode(modes_dir: Path, mode_id: str, obj: dict) -> None:
    """Write one mode JSON file (literal dict — no frontmatter, no dedent)."""
    (modes_dir / f"{mode_id}.json").write_text(
        json.dumps(obj, ensure_ascii=False), encoding="utf-8"
    )


@pytest.fixture()
def modes_dir(tmp_path, monkeypatch):
    """A throwaway modes directory wired via REFLECT_MODES_DIR, with the module's
    mode cache cleared, so every arm resolves modes ONLY from its own fresh files
    and never inherits cache state from a prior arm or the developer's repo."""
    d = tmp_path / "modes"
    d.mkdir()
    monkeypatch.setenv("REFLECT_MODES_DIR", str(d))
    # Each arm chooses the active mode explicitly; clear any stale REFLECT_MODE.
    monkeypatch.delenv("REFLECT_MODE", raising=False)
    mode_loader._MODE_CACHE.clear()
    yield d
    mode_loader._MODE_CACHE.clear()


def test_M4_child_inherits_unset_keys_and_overrides_declared(modes_dir):
    """(1) PORT ON: a single-key child mode inherits every parent key it does not
    declare and overrides the one it does. This is the shipped engineering--zh
    locale-variant shape."""
    _write_mode(
        modes_dir,
        DEFAULT_MODE,  # "engineering"
        {
            "name": "Engineering",
            "locale": "en",
            "learning_types": [
                {"id": "pattern", "label": "Pattern"},
                {"id": "bug-fix", "label": "Bug Fix"},
            ],
        },
    )
    _write_mode(modes_dir, f"{DEFAULT_MODE}--zh", {"locale": "zh"})

    merged = load_mode(f"{DEFAULT_MODE}--zh")

    # Overridden key wins.
    assert merged["locale"] == "zh", (
        f"the child declares locale=zh; the merge must override the parent's en. "
        f"got {merged.get('locale')!r}"
    )
    # Non-declared keys inherited verbatim from the parent.
    assert merged["name"] == "Engineering", (
        "name is not declared by the child, so it must be inherited from the parent"
    )
    assert [t["id"] for t in merged["learning_types"]] == ["pattern", "bug-fix"], (
        "learning_types is not declared by the child, so the parent's taxonomy "
        f"must carry through; got {merged.get('learning_types')!r}"
    )
    # The resolved id is the full leaf id.
    assert merged["id"] == f"{DEFAULT_MODE}--zh"


def test_M4_inheritance_is_exactly_one_level(modes_dir):
    """(2) FALSIFIABLE GUARD: one '--' is accepted; two ('a--b--c') is a hard
    ModeError. More than one inheritance level is forbidden (claude-mem invariant)."""
    # One level: parent named, no raise.
    assert parse_inheritance("engineering--zh") == "engineering"
    # Simple mode: no inheritance, no raise.
    assert parse_inheritance("engineering") is None

    # Two levels: rejected.
    with pytest.raises(ModeError):
        parse_inheritance("a--b--c")

    # Malformed (empty side) also rejected.
    with pytest.raises(ModeError):
        parse_inheritance("a--")


def test_M4_grandparent_key_not_pulled_through_unless_parent_redeclares(modes_dir):
    """(3) DECISIVE single-level test: a key living ONLY in a grandparent file is
    ABSENT from a leaf whose direct parent does not re-declare it, and PRESENT once
    the direct parent re-declares it. Inheritance is one level, not transitive."""
    # Grandparent file with a unique key.
    _write_mode(modes_dir, "base", {"name": "Base", "gp_only": "GRANDPARENT_ONLY"})
    # Direct parent is a SIMPLE mode that does NOT re-declare gp_only.
    _write_mode(modes_dir, "mid", {"name": "Mid", "mid_key": "FROM_PARENT"})
    # Leaf inherits the direct parent (single level).
    _write_mode(modes_dir, "mid--leaf", {"name": "Leaf", "leaf_key": "FROM_LEAF"})

    leaf = load_mode("mid--leaf")

    # Direct-parent key flows in; leaf key present; overridden name wins.
    assert leaf["mid_key"] == "FROM_PARENT", "direct parent's key must be inherited"
    assert leaf["leaf_key"] == "FROM_LEAF"
    assert leaf["name"] == "Leaf", "leaf overrides name"
    # GRANDPARENT key must NOT leak: mid does not inherit base, so base.gp_only
    # is never in mid--leaf's chain.
    assert "gp_only" not in leaf, (
        "single-level inheritance must NOT pull a grandparent-only key through; "
        f"got leaked {leaf.get('gp_only')!r}. keys: {sorted(leaf.keys())}"
    )

    # Contrast: re-declare gp_only on the DIRECT parent -> it now flows through.
    mode_loader._MODE_CACHE.clear()
    _write_mode(
        modes_dir,
        "mid",
        {"name": "Mid", "mid_key": "FROM_PARENT", "gp_only": "REDECLARED_BY_PARENT"},
    )
    leaf2 = load_mode("mid--leaf")
    assert leaf2["gp_only"] == "REDECLARED_BY_PARENT", (
        "once the DIRECT parent re-declares the key it must reach the leaf — "
        "this contrast pins that the absence above was the single-level rule, "
        "not a missing file"
    )


def test_M4_deep_merge_replaces_lists_wholesale_and_merges_dicts(modes_dir):
    """Cross-check on the merge primitive the port ships: nested dicts merge
    key-by-key while lists/scalars REPLACE wholesale (claude-mem deepMerge
    semantics). This is what makes a one-key locale override a full mode."""
    base = {
        "locale": "en",
        "prompts": {"drain_writer": "WRITE", "skill_refresh": "REFRESH"},
        "learning_types": [{"id": "pattern"}],
    }
    override = {
        "locale": "zh",  # scalar replace
        "prompts": {"drain_writer": "写"},  # dict merge: drain_writer replaced, skill_refresh kept
        "learning_types": [{"id": "legal-precedent"}],  # list replace, not append
    }
    merged = deep_merge(base, override)

    assert merged["locale"] == "zh"
    assert merged["prompts"]["drain_writer"] == "写", "declared sub-key replaced"
    assert merged["prompts"]["skill_refresh"] == "REFRESH", (
        "undeclared sub-key kept — dicts merge key-by-key"
    )
    assert merged["learning_types"] == [{"id": "legal-precedent"}], (
        "lists replace wholesale; the parent's list must NOT be appended to"
    )
    # deep_merge must not mutate its inputs.
    assert base["locale"] == "en", "deep_merge must not mutate the base"
    assert base["prompts"]["drain_writer"] == "WRITE"


def test_M4_reflect_mode_env_switches_resolved_config(modes_dir, monkeypatch):
    """(4) THE ENV KNOB: REFLECT_MODE selects the active mode. Same files; flipping
    the env var alone changes the resolved locale AND the derived LANGUAGE
    REQUIREMENTS directive. This proves the PORT (env-driven resolution) caused
    the difference, not text luck."""
    _write_mode(
        modes_dir,
        DEFAULT_MODE,
        {"name": "Engineering", "locale": "en", "learning_types": [{"id": "pattern"}]},
    )
    _write_mode(modes_dir, f"{DEFAULT_MODE}--zh", {"locale": "zh"})

    # KNOB = engineering (default mode).
    monkeypatch.setenv("REFLECT_MODE", DEFAULT_MODE)
    mode_loader._MODE_CACHE.clear()
    assert resolve_active_mode_id() == DEFAULT_MODE
    eng = get_active_mode()
    assert get_locale(eng) == "en"
    assert language_requirement(get_locale(eng), eng) == "", (
        "English locale derives an EMPTY language directive"
    )

    # KNOB flipped = engineering--zh (locale variant, inheriting the parent).
    monkeypatch.setenv("REFLECT_MODE", f"{DEFAULT_MODE}--zh")
    mode_loader._MODE_CACHE.clear()
    assert resolve_active_mode_id() == f"{DEFAULT_MODE}--zh"
    zh = get_active_mode()
    assert get_locale(zh) == "zh", "flipping REFLECT_MODE must change the resolved locale"
    directive = language_requirement(get_locale(zh), zh)
    assert directive and "LANGUAGE REQUIREMENTS" in directive, (
        "the zh variant must derive a non-empty LANGUAGE REQUIREMENTS directive"
    )
    # And the inherited taxonomy still carries through under the switched mode.
    assert [t["id"] for t in zh["learning_types"]] == ["pattern"], (
        "the switched-to child still inherits the parent's learning_types"
    )
    # The two resolved configs differ on the knob-controlled field.
    assert get_locale(eng) != get_locale(zh), (
        "REFLECT_MODE alone changed the resolved config; the knob is decisive"
    )


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
