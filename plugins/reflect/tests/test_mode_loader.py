# ABOUTME: Regression tests for port M4 — pluggable mode system with single-level parent--override inheritance.
# ABOUTME: Pins every M4 acceptance bullet: inheritance merge, engineering fidelity, locale-only zh variant, set-persistence, runtime pull.
"""Port M4 (pattern: claude-mem ModeManager + plugin/modes/*.json):
declarative JSON mode files bundle learning types, concept tags, signal
patterns, prompt templates, and locale, with single-level inheritance via
parent--override naming. The default `engineering` mode expresses the
historical reflect taxonomy with zero behaviour change; `engineering--zh`
shows a locale-only variant producing a Chinese-prompt writer.

Acceptance bullets pinned here:
1. mode-loader resolves parent--override inheritance -> merged mode object
2. engineering.json faithfully expresses the existing taxonomy (zero change)
3. engineering--zh.json with ONLY a locale field -> Chinese-prompt variant
4. `mode_loader.py set <id>` persists into .reflect/config.json and is picked
   up by the next process ("next session")
5. reviewer (signal_detector) + writer (drain prompt) pull types, concepts,
   prompt template, and locale from the active mode at runtime
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
SCRIPTS = PLUGIN_ROOT / "scripts"
MODES_DIR = PLUGIN_ROOT / "references" / "modes"
MODE_LOADER = SCRIPTS / "mode_loader.py"
DRAIN = PLUGIN_ROOT / "hooks" / "reflect-drain-bg.sh"

sys.path.insert(0, str(SCRIPTS))

import mode_loader  # noqa: E402
import signal_detector  # noqa: E402
from mode_loader import (  # noqa: E402
    DEFAULT_MODE,
    ModeError,
    deep_merge,
    drain_prompt,
    get_concepts,
    get_learning_types,
    get_locale,
    language_requirement,
    list_modes,
    load_mode,
    parse_inheritance,
    render_prompt,
    resolve_active_mode_id,
)


# ---------------------------------------------------------------------------
# Expected engineering prompts — byte-identical to the historical inline
# construction in reflect-drain-bg.sh (zero behaviour change pin).
# ---------------------------------------------------------------------------

SPECULATIVE_NOTE = """

This reflection was triggered by session IDLENESS (no transcript activity for the idle window), NOT an explicit session end — the session may still resume and overturn these conclusions. Treat every finding as provisional: add the tag 'speculative' to the tags list of EVERY learning you write, and cap confidence at MEDIUM (never HIGH)."""


def expected_writer_prompt(target: str, speculative_note: str = "") -> str:
    return f"""/reflect

Process the transcript at: {target}

Extract any HIGH-confidence corrections, MEDIUM-confidence approved approaches, and noteworthy patterns. Write each as a learning document via the standard reflect workflow.

Belief revision: if the input contains a 'Related existing learnings' section, prefer UPDATE over CREATE — when a finding restates a listed learning, do NOT write a duplicate note; emit the UPDATE action (or DELETE, only for a learning the new evidence directly contradicts or supersedes) and execute it with the exact 'revise' command shown in that section.{speculative_note}

When done, summarize what you captured. Do NOT touch the queue file — the drain script handles archiving."""


def expected_skill_refresh_prompt(
    skill_name: str, transcript: str, learning_id: str, reason: str
) -> str:
    return f"""/reflect

Skill refresh (auto-triggered): the skill '{skill_name}' at {transcript} is marked stale — a learning that backs it was revised (learning: {learning_id}; reason: {reason}).

Re-run the skill-edit step on this skill: read the SKILL.md, check the current learnings covering its domain (reflect search, or the learnings table in reflect.db), and EDIT the SKILL.md in place so its guidance matches the revised corpus — fold in the new rule, and update or remove any guidance the revision contradicts. Keep the edit surgical and additive where possible; do not rewrite unrelated sections.

When done, summarize what you changed. Do NOT touch the queue file — the drain script handles archiving."""


@pytest.fixture(autouse=True)
def _clean_mode_env(monkeypatch):
    """Isolate every test from the host's mode selection + caches."""
    monkeypatch.delenv("REFLECT_MODE", raising=False)
    monkeypatch.delenv("REFLECT_MODES_DIR", raising=False)
    monkeypatch.delenv("CLAUDE_PROJECT_DIR", raising=False)
    mode_loader._MODE_CACHE.clear()
    signal_detector._reset_pattern_cache()
    yield
    mode_loader._MODE_CACHE.clear()
    signal_detector._reset_pattern_cache()


def _write_mode(dir_: Path, mode_id: str, data: dict) -> Path:
    path = dir_ / f"{mode_id}.json"
    path.write_text(json.dumps(data, ensure_ascii=False), encoding="utf-8")
    return path


@pytest.fixture
def custom_modes(tmp_path, monkeypatch):
    """A throwaway modes dir selected via REFLECT_MODES_DIR."""
    modes = tmp_path / "modes"
    modes.mkdir()
    # The loader's hard fallback target must exist in any modes dir.
    _write_mode(modes, DEFAULT_MODE, {
        "name": "Engineering (stub)",
        "locale": "en",
        "learning_types": [{"id": "pattern", "description": "stub"}],
        "concepts": [{"id": "tools", "label": "Tools"}],
        "prompts": {"drain_writer": "ENGINEERING {target}",
                    "skill_refresh": "REFRESH {skill_name}",
                    "speculative_idle": " IDLE"},
    })
    monkeypatch.setenv("REFLECT_MODES_DIR", str(modes))
    return modes


# ===========================================================================
# Bullet 1 — inheritance resolution returns a merged mode object
# ===========================================================================


class TestInheritance:
    def test_parse_inheritance(self):
        assert parse_inheritance("engineering") is None
        assert parse_inheritance("engineering--zh") == "engineering"

    def test_more_than_one_level_is_an_error(self):
        with pytest.raises(ModeError):
            parse_inheritance("a--b--c")

    def test_deep_merge_semantics(self):
        base = {"a": 1, "nested": {"x": 1, "y": 2}, "list": [1, 2]}
        override = {"a": 9, "nested": {"y": 99}, "list": [3]}
        merged = deep_merge(base, override)
        assert merged == {"a": 9, "nested": {"x": 1, "y": 99}, "list": [3]}
        assert base["nested"] == {"x": 1, "y": 2}  # base untouched

    def test_parent_override_returns_merged_object(self, custom_modes):
        _write_mode(custom_modes, "law", {
            "name": "Law Study",
            "locale": "en",
            "learning_types": [{"id": "holding"}, {"id": "dissent"}],
            "prompts": {"drain_writer": "LAW {target}", "skill_refresh": "R",
                        "speculative_idle": ""},
        })
        _write_mode(custom_modes, "law--fr", {
            "locale": "fr",
            "prompts": {"drain_writer": "DROIT {target}"},
        })
        merged = load_mode("law--fr")
        assert merged["id"] == "law--fr"
        assert merged["name"] == "Law Study"                      # inherited
        assert merged["locale"] == "fr"                            # overridden
        assert [t["id"] for t in merged["learning_types"]] == ["holding", "dissent"]
        assert merged["prompts"]["drain_writer"] == "DROIT {target}"  # deep merge
        assert merged["prompts"]["skill_refresh"] == "R"               # sibling kept

    def test_missing_override_file_yields_parent(self, custom_modes):
        mode = load_mode("engineering--nofile")
        assert mode["name"] == "Engineering (stub)"

    def test_unknown_mode_falls_back_to_default(self, custom_modes):
        mode = load_mode("does-not-exist")
        assert mode["id"] == DEFAULT_MODE

    def test_missing_default_mode_is_fatal(self, tmp_path, monkeypatch):
        empty = tmp_path / "empty-modes"
        empty.mkdir()
        monkeypatch.setenv("REFLECT_MODES_DIR", str(empty))
        with pytest.raises(ModeError):
            load_mode(DEFAULT_MODE)


# ===========================================================================
# Bullet 2 — engineering.json faithfully expresses the current taxonomy
# ===========================================================================


class TestEngineeringFidelity:
    def test_engineering_is_the_default_active_mode(self):
        assert resolve_active_mode_id() == "engineering"
        cfg = __import__("reflect_config").load_config(force_reload=True)
        assert cfg.get("mode") == "engineering"

    def test_learning_types_match_note_taxonomy(self):
        mode = load_mode("engineering")
        ids = [t["id"] for t in get_learning_types(mode)]
        # note_templates.md: pattern | correction | bug-fix | decision | anti-pattern
        assert sorted(ids) == sorted(
            ["pattern", "correction", "bug-fix", "decision", "anti-pattern"]
        )

    def test_concepts_match_signal_detector_categories(self):
        mode = load_mode("engineering")
        concept_ids = {c["id"] for c in get_concepts(mode)}
        expected = {
            cat.value.lower().replace(" ", "-")
            for cat in signal_detector.Category
            if cat is not signal_detector.Category.UNKNOWN
        }
        assert concept_ids == expected

    def test_signal_patterns_equal_builtin_pattern_sets(self):
        sp = load_mode("engineering")["signal_patterns"]
        assert [tuple(p) for p in sp["high"]] == signal_detector.HIGH_PATTERNS
        assert [tuple(p) for p in sp["medium"]] == signal_detector.MEDIUM_PATTERNS
        assert [tuple(p) for p in sp["low"]] == signal_detector.LOW_PATTERNS
        builtin_cats = {
            cat.value.lower().replace(" ", "-"): pats
            for cat, pats in signal_detector.CATEGORY_PATTERNS.items()
        }
        assert sp["categories"] == builtin_cats

    def test_drain_prompt_byte_identical_to_inline_bash_prompt(self):
        target = "/tmp/slice.jsonl"
        assert drain_prompt(target, "stop") == expected_writer_prompt(target)
        assert drain_prompt(target, "precompact") == expected_writer_prompt(target)

    def test_idle_prompt_carries_speculative_note(self):
        target = "/tmp/t.jsonl"
        assert drain_prompt(target, "idle") == expected_writer_prompt(
            target, SPECULATIVE_NOTE
        )

    def test_skill_refresh_prompt_byte_identical_including_bash_defaults(self):
        got = drain_prompt(
            "/tmp/SKILL.md", "skill_refresh",
            skill_name="webapp-testing", transcript="/tmp/SKILL.md",
            learning_id="lrn-x-abc123", reason="belief revision: superseded",
        )
        assert got == expected_skill_refresh_prompt(
            "webapp-testing", "/tmp/SKILL.md", "lrn-x-abc123",
            "belief revision: superseded",
        )
        # Empty optional fields take the same defaults bash used (${var:-...}).
        got = drain_prompt("/tmp/SKILL.md", "skill_refresh", transcript="/tmp/SKILL.md")
        assert got == expected_skill_refresh_prompt(
            "unknown", "/tmp/SKILL.md", "unknown", "belief revision"
        )

    def test_signal_detector_behaviour_unchanged_under_engineering(self):
        # Mode-driven pattern sets resolve to exactly the builtins.
        high, medium, low, cats = signal_detector._pattern_sets()
        assert high == signal_detector.HIGH_PATTERNS
        assert medium == signal_detector.MEDIUM_PATTERNS
        assert low == signal_detector.LOW_PATTERNS
        assert cats == signal_detector.CATEGORY_PATTERNS
        # And the detector's own self-test corpus still passes end-to-end.
        sigs = signal_detector.detect_signals("Never use var in TypeScript")
        assert sigs[0].confidence is signal_detector.Confidence.HIGH
        assert sigs[0].category is signal_detector.Category.CODE_STYLE


# ===========================================================================
# Bullet 3 — locale-only engineering--zh.json -> Chinese-prompt variant
# ===========================================================================


class TestLocaleVariant:
    def test_shipped_zh_variant_has_only_a_locale_field(self):
        data = json.loads((MODES_DIR / "engineering--zh.json").read_text())
        assert data == {"locale": "zh"}

    def test_zh_variant_inherits_full_engineering_taxonomy(self):
        zh = load_mode("engineering--zh")
        eng = load_mode("engineering")
        assert get_locale(zh) == "zh"
        assert get_learning_types(zh) == get_learning_types(eng)
        assert get_concepts(zh) == get_concepts(eng)
        assert zh["signal_patterns"] == eng["signal_patterns"]

    def test_zh_variant_produces_chinese_prompt(self):
        target = "/tmp/t.jsonl"
        zh = load_mode("engineering--zh")
        got = drain_prompt(target, "stop", mode=zh)
        directive = language_requirement("zh")
        assert "中文" in directive
        assert got == expected_writer_prompt(target) + "\n\n" + directive

    def test_english_locale_appends_nothing(self):
        assert language_requirement("en") == ""
        assert language_requirement("") == ""

    def test_unknown_locale_gets_generic_directive(self):
        assert "locale code 'xx'" in language_requirement("xx")


# ===========================================================================
# Bullet 4 — `set <id>` persists into .reflect/config.json, next session wins
# ===========================================================================


def _cli(args: list[str], project_dir: Path, extra_env: dict | None = None):
    env = {k: v for k, v in os.environ.items()
           if k not in ("REFLECT_MODE", "REFLECT_MODES_DIR")}
    env["CLAUDE_PROJECT_DIR"] = str(project_dir)
    env.update(extra_env or {})
    return subprocess.run(
        [sys.executable, str(MODE_LOADER), *args],
        capture_output=True, text=True, env=env, cwd=str(project_dir),
    )


class TestSetPersistence:
    def test_set_writes_reflect_config_json(self, tmp_path):
        res = _cli(["set", "engineering--zh"], tmp_path)
        assert res.returncode == 0, res.stderr
        cfg = json.loads((tmp_path / ".reflect" / "config.json").read_text())
        assert cfg["mode"] == "engineering--zh"
        assert "mode_updated_at" in cfg

    def test_choice_is_picked_up_on_next_session(self, tmp_path):
        assert _cli(["set", "engineering--zh"], tmp_path).returncode == 0
        # A fresh process ("next session") resolves the persisted mode...
        res = _cli(["get"], tmp_path)
        assert res.returncode == 0
        assert res.stdout.strip() == "engineering--zh"
        # ...and the writer prompt it renders is the Chinese variant.
        res = _cli(["drain-prompt", "--target", "/tmp/t.jsonl", "--trigger", "stop"],
                   tmp_path)
        assert res.returncode == 0
        assert "中文" in res.stdout

    def test_set_preserves_other_config_keys(self, tmp_path):
        cfg_dir = tmp_path / ".reflect"
        cfg_dir.mkdir()
        (cfg_dir / "config.json").write_text('{"other": "keep-me"}')
        assert _cli(["set", "engineering--zh"], tmp_path).returncode == 0
        cfg = json.loads((cfg_dir / "config.json").read_text())
        assert cfg["other"] == "keep-me"
        assert cfg["mode"] == "engineering--zh"

    def test_set_rejects_unknown_mode(self, tmp_path):
        res = _cli(["set", "no-such-mode"], tmp_path)
        assert res.returncode == 1
        assert not (tmp_path / ".reflect" / "config.json").exists()

    def test_env_var_beats_persisted_choice(self, tmp_path, monkeypatch):
        assert _cli(["set", "engineering--zh"], tmp_path).returncode == 0
        res = _cli(["get"], tmp_path, extra_env={"REFLECT_MODE": "engineering"})
        assert res.stdout.strip() == "engineering"

    def test_default_when_nothing_is_set(self, tmp_path):
        res = _cli(["get"], tmp_path)
        assert res.stdout.strip() == "engineering"


# ===========================================================================
# Bullet 5 — reviewer + writer pull types/concepts/prompt/locale at runtime
# ===========================================================================


class TestRuntimePull:
    def test_reviewer_uses_active_mode_signal_patterns(self, custom_modes, monkeypatch):
        _write_mode(custom_modes, "legal", {
            "name": "Legal",
            "locale": "en",
            "learning_types": [{"id": "holding"}],
            "concepts": [{"id": "process", "label": "Process"}],
            "signal_patterns": {
                "high": [["\\b(the court held)\\b", "holding"]],
                "medium": [], "low": [],
                "categories": {"process": ["\\b(appeal|motion)\\b"]},
            },
            "prompts": {"drain_writer": "L {target}", "skill_refresh": "R",
                        "speculative_idle": ""},
        })
        monkeypatch.setenv("REFLECT_MODE", "legal")
        signal_detector._reset_pattern_cache()

        conf, ptype = signal_detector.detect_confidence("The court held that X")
        assert conf is signal_detector.Confidence.HIGH
        assert ptype == "holding"
        # Engineering's HIGH pattern is gone in this mode.
        conf, _ = signal_detector.detect_confidence("never do this again ok")
        assert conf is signal_detector.Confidence.LOW
        # Concept patterns map onto the matching Category at runtime.
        assert signal_detector.detect_category("filed a motion to dismiss") \
            is signal_detector.Category.PROCESS

    def test_reviewer_cache_follows_mode_switches(self, custom_modes, monkeypatch):
        signal_detector._reset_pattern_cache()
        high, *_ = signal_detector._pattern_sets()  # engineering stub
        monkeypatch.setenv("REFLECT_MODE", "engineering")
        assert signal_detector._pattern_sets()[0] == high

    def test_writer_prompt_renders_mode_types_and_concepts(self, custom_modes, monkeypatch):
        _write_mode(custom_modes, "study", {
            "name": "Study",
            "locale": "ja",
            "learning_types": [
                {"id": "definition", "description": "A precise definition"},
                {"id": "doctrine", "description": "A legal doctrine"},
            ],
            "concepts": [{"id": "exam-trap", "description": "Classic exam trap"}],
            "prompts": {
                "drain_writer": "Study {target}\nTYPES:\n{types}\nCONCEPTS:\n{concepts}",
                "skill_refresh": "R",
                "speculative_idle": "",
            },
        })
        monkeypatch.setenv("REFLECT_MODE", "study")
        got = drain_prompt("/tmp/x.jsonl", "stop")
        assert "Study /tmp/x.jsonl" in got
        assert "- definition: A precise definition" in got
        assert "- doctrine: A legal doctrine" in got
        assert "- exam-trap: Classic exam trap" in got
        assert "日本語" in got  # locale pulled at render time

    def test_drain_script_invokes_mode_loader_for_the_writer_prompt(self):
        text = DRAIN.read_text()
        assert "mode_loader.py" in text
        assert "drain-prompt" in text

    def test_drain_prompt_cli_matches_library_render(self, tmp_path):
        res = _cli(["drain-prompt", "--target", "/tmp/t.jsonl", "--trigger", "idle"],
                   tmp_path)
        assert res.returncode == 0
        assert res.stdout == expected_writer_prompt("/tmp/t.jsonl", SPECULATIVE_NOTE)

    def test_reflect_mode_env_overrides_config_cascade(self, monkeypatch):
        monkeypatch.setenv("REFLECT_MODE", "engineering--zh")
        reflect_config = __import__("reflect_config")
        cfg = reflect_config.load_config(force_reload=True)
        assert cfg["mode"] == "engineering--zh"
        assert resolve_active_mode_id() == "engineering--zh"
        # Restore the cached config for other tests in this process.
        monkeypatch.delenv("REFLECT_MODE")
        reflect_config.load_config(force_reload=True)

    def test_render_prompt_leaves_unknown_placeholders_intact(self, custom_modes):
        _write_mode(custom_modes, "raw", {
            "name": "Raw", "locale": "en",
            "prompts": {"drain_writer": "{target} and {mystery}",
                        "skill_refresh": "R", "speculative_idle": ""},
        })
        got = render_prompt("drain_writer", mode=load_mode("raw"), target="T")
        assert got == "T and {mystery}"

    def test_list_modes_includes_shipped_modes(self):
        ids = list_modes()
        assert "engineering" in ids
        assert "engineering--zh" in ids


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
