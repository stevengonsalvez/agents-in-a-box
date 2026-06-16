#!/usr/bin/env python3
# ABOUTME: Pluggable mode loader with single-level parent--override inheritance (port M4, pattern from claude-mem).
# ABOUTME: A mode JSON bundles learning types, concepts, signal patterns, prompt templates, and locale; default=engineering.
"""Mode loader for the reflect plugin (port M4).

Pattern source: claude-mem's ModeManager (``src/services/domain/ModeManager.ts``)
plus its ``plugin/modes/*.json`` mode files. Clean-room reimplementation adapted
to reflect's taxonomy: a *mode* is a declarative JSON file under
``plugins/reflect/references/modes/`` that bundles

* ``learning_types``   — the note taxonomy (pattern / correction / bug-fix /
                         decision / anti-pattern in the default engineering mode)
* ``concepts``         — the category tags the reviewer classifies signals into
* ``signal_patterns``  — the regex pattern sets signal_detector.py gates with
* ``prompts``          — the drain writer / skill-refresh prompt templates
* ``locale``           — output language (non-``en`` locales append a LANGUAGE
                         REQUIREMENTS directive to every rendered prompt)

Inheritance is single-level via ``parent--override`` file naming: loading
``engineering--zh`` deep-merges ``engineering--zh.json`` on top of
``engineering.json``, so a locale variant can be a one-key file. More than one
``--`` is an error (claude-mem invariant). Unknown modes fall back to the
default ``engineering`` mode so a typo can never break the pipeline.

Active-mode resolution (later wins is *earlier* in this list):

1. ``REFLECT_MODE`` env var
2. ``{project}/.reflect/config.json`` ``"mode"`` key (written by ``set``)
3. ``mode`` key in the reflect_config 4-layer TOML cascade
4. built-in default ``engineering``

CLI:
    mode_loader.py list                       # available mode ids
    mode_loader.py get                        # active mode id
    mode_loader.py set <mode_id>              # persist into .reflect/config.json
    mode_loader.py show [mode_id]             # merged mode object as JSON
    mode_loader.py drain-prompt --target T --trigger TR
        [--skill-name S] [--transcript P] [--learning-id L] [--reason R]
"""

from __future__ import annotations

import json
import os
import sys
from datetime import datetime
from pathlib import Path
from typing import Any, Optional

# Ensure sibling imports work when run standalone
_SCRIPTS_DIR = Path(__file__).resolve().parent
if str(_SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS_DIR))

from reflect_config import get_config

DEFAULT_MODE = "engineering"

# Locale -> human-readable language name for the auto-derived LANGUAGE
# REQUIREMENTS directive. A locale variant mode therefore only needs a
# ``locale`` field — the loader supplies the prompt suffix. Unknown locales
# get a generic BCP-47 phrasing rather than failing.
LOCALE_LANGUAGES: dict[str, str] = {
    "en": "",  # default — no directive appended
    "zh": "中文 (Chinese)",
    "ja": "日本語 (Japanese)",
    "ko": "한국어 (Korean)",
    "es": "Español (Spanish)",
    "fr": "Français (French)",
    "de": "Deutsch (German)",
    "it": "Italiano (Italian)",
    "pt": "Português (Portuguese)",
    "pt-br": "Português do Brasil (Brazilian Portuguese)",
    "ru": "Русский (Russian)",
    "nl": "Nederlands (Dutch)",
    "hi": "हिन्दी (Hindi)",
    "ar": "العربية (Arabic)",
    "tr": "Türkçe (Turkish)",
    "pl": "Polski (Polish)",
    "vi": "Tiếng Việt (Vietnamese)",
}

# Loaded-mode cache, keyed by (modes_dir, mode_id). The active-mode *id* is
# re-resolved on every get_active_mode() call (env / config.json may change),
# but a given mode file pair is only parsed once per process.
_MODE_CACHE: dict[tuple[str, str], dict[str, Any]] = {}


class ModeError(Exception):
    """Raised for unrecoverable mode problems (bad inheritance, missing default)."""


# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------


def modes_dir() -> Path:
    """Directory holding ``*.json`` mode files (env REFLECT_MODES_DIR override)."""
    env = os.environ.get("REFLECT_MODES_DIR", "").strip()
    if env:
        return Path(env).expanduser()
    return _SCRIPTS_DIR.parent / "references" / "modes"


def _mode_file_path(mode_id: str) -> Path:
    return modes_dir() / f"{mode_id}.json"


def _project_dir() -> Path:
    """Project root: $CLAUDE_PROJECT_DIR, else nearest .git ancestor, else cwd."""
    env = os.environ.get("CLAUDE_PROJECT_DIR", "").strip()
    if env:
        return Path(env).expanduser()
    cwd = Path.cwd()
    for parent in [cwd] + list(cwd.parents):
        if (parent / ".git").exists():
            return parent
    return cwd


def _project_config_path() -> Path:
    """Per-project persisted choice: ``{project}/.reflect/config.json``."""
    return _project_dir() / ".reflect" / "config.json"


# ---------------------------------------------------------------------------
# Inheritance + merge
# ---------------------------------------------------------------------------


def parse_inheritance(mode_id: str) -> Optional[str]:
    """Return the parent id for a ``parent--override`` mode id, or None.

    Raises ModeError when more than one inheritance level is requested
    (``a--b--c``) — claude-mem supports exactly one level and so do we.
    """
    parts = mode_id.split("--")
    if len(parts) == 1:
        return None
    if len(parts) > 2 or not parts[0] or not parts[1]:
        raise ModeError(
            f"Invalid mode inheritance: {mode_id!r}. "
            "Only one level of inheritance is supported (parent--override)."
        )
    return parts[0]


def deep_merge(base: dict, override: dict) -> dict:
    """Recursively merge *override* into a copy of *base*.

    Dicts merge key-by-key; lists and scalars replace wholesale (an override
    that restates ``learning_types`` replaces the parent's list, it does not
    append to it — same semantics as claude-mem's ModeManager.deepMerge).
    """
    merged = base.copy()
    for key, value in override.items():
        if key in merged and isinstance(merged[key], dict) and isinstance(value, dict):
            merged[key] = deep_merge(merged[key], value)
        else:
            merged[key] = value
    return merged


def _load_mode_file(mode_id: str) -> dict[str, Any]:
    """Load one raw mode JSON file. Raises on missing/invalid content."""
    path = _mode_file_path(mode_id)
    if not path.is_file():
        raise FileNotFoundError(f"Mode file not found: {path}")
    with open(path, encoding="utf-8") as fh:
        data = json.load(fh)
    if not isinstance(data, dict):
        raise ModeError(f"Mode file is not a JSON object: {path}")
    return data


def load_mode(mode_id: str) -> dict[str, Any]:
    """Resolve *mode_id* (with single-level inheritance) to a merged mode dict.

    Fallback chain mirrors claude-mem: an unknown simple mode falls back to
    the default ``engineering`` mode; an unknown parent falls back to the
    default; a missing override file yields the parent alone. Only a missing
    ``engineering.json`` is fatal.
    """
    cache_key = (str(modes_dir()), mode_id)
    if cache_key in _MODE_CACHE:
        return _MODE_CACHE[cache_key]

    parent_id = parse_inheritance(mode_id)

    if parent_id is None:
        try:
            mode = _load_mode_file(mode_id)
        except (FileNotFoundError, json.JSONDecodeError, ModeError):
            if mode_id == DEFAULT_MODE:
                raise ModeError(
                    f"Critical: default mode file missing or invalid: "
                    f"{_mode_file_path(DEFAULT_MODE)}"
                )
            return load_mode(DEFAULT_MODE)
        mode = dict(mode)
        mode["id"] = mode_id
        _MODE_CACHE[cache_key] = mode
        return mode

    parent = load_mode(parent_id)

    try:
        override = _load_mode_file(mode_id)
    except (FileNotFoundError, json.JSONDecodeError, ModeError):
        # Missing/broken override -> parent alone (claude-mem behaviour).
        return parent

    merged = deep_merge(parent, override)
    merged["id"] = mode_id
    _MODE_CACHE[cache_key] = merged
    return merged


def list_modes() -> list[str]:
    """Sorted ids of every mode file on disk."""
    try:
        return sorted(p.stem for p in modes_dir().glob("*.json"))
    except OSError:
        return []


# ---------------------------------------------------------------------------
# Active-mode resolution + persistence
# ---------------------------------------------------------------------------


def resolve_active_mode_id() -> str:
    """Active mode id: env > project .reflect/config.json > TOML cascade > default."""
    env = os.environ.get("REFLECT_MODE", "").strip()
    if env:
        return env

    try:
        cfg_path = _project_config_path()
        if cfg_path.is_file():
            with open(cfg_path, encoding="utf-8") as fh:
                data = json.load(fh)
            mode = str(data.get("mode", "") or "").strip()
            if mode:
                return mode
    except Exception:
        pass  # silent-fail: a broken config.json must never break the pipeline

    try:
        mode = str(get_config().get("mode", "") or "").strip()
        if mode:
            return mode
    except Exception:
        pass

    return DEFAULT_MODE


def get_active_mode() -> dict[str, Any]:
    """Merged mode object for the currently-active mode id."""
    return load_mode(resolve_active_mode_id())


def set_active_mode(mode_id: str) -> Path:
    """Persist *mode_id* into ``{project}/.reflect/config.json`` (read-modify-write).

    Strict, unlike load_mode: refuses ids whose mode file does not exist so a
    typo is caught at `set` time rather than silently falling back later.
    Returns the config path written.
    """
    parse_inheritance(mode_id)  # raises on a--b--c
    if not _mode_file_path(mode_id).is_file():
        raise ModeError(
            f"Unknown mode {mode_id!r}: no such file {_mode_file_path(mode_id)} "
            f"(available: {', '.join(list_modes()) or 'none'})"
        )

    cfg_path = _project_config_path()
    data: dict[str, Any] = {}
    if cfg_path.is_file():
        try:
            with open(cfg_path, encoding="utf-8") as fh:
                loaded = json.load(fh)
            if isinstance(loaded, dict):
                data = loaded
        except Exception:
            data = {}  # unreadable -> rewrite with just our keys

    data["mode"] = mode_id
    data["mode_updated_at"] = datetime.now().isoformat()

    cfg_path.parent.mkdir(parents=True, exist_ok=True)
    with open(cfg_path, "w", encoding="utf-8") as fh:
        json.dump(data, fh, indent=2, ensure_ascii=False)
        fh.write("\n")
    return cfg_path


# ---------------------------------------------------------------------------
# Accessors (the surface reviewer/writer code pulls from at runtime)
# ---------------------------------------------------------------------------


def get_learning_types(mode: Optional[dict] = None) -> list[dict[str, Any]]:
    mode = mode if mode is not None else get_active_mode()
    types = mode.get("learning_types", [])
    return types if isinstance(types, list) else []


def get_concepts(mode: Optional[dict] = None) -> list[dict[str, Any]]:
    mode = mode if mode is not None else get_active_mode()
    concepts = mode.get("concepts", [])
    return concepts if isinstance(concepts, list) else []


def get_locale(mode: Optional[dict] = None) -> str:
    mode = mode if mode is not None else get_active_mode()
    return str(mode.get("locale", "en") or "en").strip().lower()


def get_signal_patterns(mode: Optional[dict] = None) -> dict[str, Any]:
    mode = mode if mode is not None else get_active_mode()
    patterns = mode.get("signal_patterns", {})
    return patterns if isinstance(patterns, dict) else {}


def language_requirement(locale: str, mode: Optional[dict] = None) -> str:
    """The LANGUAGE REQUIREMENTS directive for *locale* ('' for English).

    A mode may hand-write its own directive via ``prompts.language_requirements``
    (which wins, even when empty); otherwise the directive is derived from the
    LOCALE_LANGUAGES table so a locale-only variant file is sufficient.
    """
    if mode is not None:
        prompts = mode.get("prompts", {})
        if isinstance(prompts, dict) and "language_requirements" in prompts:
            return str(prompts["language_requirements"] or "")
    locale = (locale or "en").strip().lower()
    language = LOCALE_LANGUAGES.get(locale)
    if language == "":
        return ""
    if language is None:
        if locale in ("", "en") or locale.startswith("en-"):
            return ""
        language = f"the language with locale code '{locale}'"
    return (
        "LANGUAGE REQUIREMENTS: Write all learning content (titles, summaries, "
        f"problems, solutions, key insights) in {language}."
    )


class _SafeDict(dict):
    """format_map helper: unknown placeholders pass through unrendered."""

    def __missing__(self, key: str) -> str:  # pragma: no cover - trivial
        return "{" + key + "}"


def _format_taxonomy(entries: list[dict[str, Any]]) -> str:
    """Render types/concepts as a bullet list for {types}/{concepts} placeholders."""
    lines = []
    for entry in entries:
        if not isinstance(entry, dict):
            continue
        eid = str(entry.get("id", "")).strip()
        desc = str(entry.get("description", "") or entry.get("label", "")).strip()
        if eid:
            lines.append(f"- {eid}: {desc}" if desc else f"- {eid}")
    return "\n".join(lines)


def render_prompt(name: str, mode: Optional[dict] = None, **fields: str) -> str:
    """Render the mode's prompt template *name* with placeholder substitution.

    Available placeholders: whatever the caller passes plus {types} and
    {concepts} (auto-rendered from the mode taxonomy). Unknown placeholders
    are left intact. Non-English locales get the LANGUAGE REQUIREMENTS
    directive appended, so a locale-only override file changes every prompt.
    """
    mode = mode if mode is not None else get_active_mode()
    prompts = mode.get("prompts", {})
    template = prompts.get(name, "") if isinstance(prompts, dict) else ""
    if not template:
        raise ModeError(f"Mode {mode.get('id', '?')!r} has no prompt template {name!r}")

    values = _SafeDict(fields)
    values.setdefault("types", _format_taxonomy(get_learning_types(mode)))
    values.setdefault("concepts", _format_taxonomy(get_concepts(mode)))
    rendered = str(template).format_map(values)

    directive = language_requirement(get_locale(mode), mode)
    if directive:
        rendered = f"{rendered}\n\n{directive}"
    return rendered


def drain_prompt(
    target: str,
    trigger: str,
    skill_name: str = "",
    transcript: str = "",
    learning_id: str = "",
    reason: str = "",
    mode: Optional[dict] = None,
) -> str:
    """The drain writer prompt for one queue entry, from the active mode.

    Mirrors the historical inline construction in reflect-drain-bg.sh exactly
    for the default engineering mode (zero behaviour change): skill_refresh
    entries get the skill-edit prompt; idle entries get the speculative
    addendum; everything else gets the plain writer prompt.
    """
    mode = mode if mode is not None else get_active_mode()

    if trigger == "skill_refresh":
        return render_prompt(
            "skill_refresh",
            mode=mode,
            skill_name=skill_name or "unknown",
            transcript=transcript or target,
            learning_id=learning_id or "unknown",
            reason=reason or "belief revision",
        )

    speculative_note = ""
    if trigger == "idle":
        prompts = mode.get("prompts", {})
        speculative_note = (
            prompts.get("speculative_idle", "") if isinstance(prompts, dict) else ""
        )
    return render_prompt(
        "drain_writer", mode=mode, target=target, speculative_note=speculative_note
    )


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def main() -> None:
    import argparse

    ap = argparse.ArgumentParser(description="Reflect mode loader (port M4)")
    sub = ap.add_subparsers(dest="cmd", required=True)

    sub.add_parser("list", help="list available mode ids")
    sub.add_parser("get", help="print the active mode id")

    sp = sub.add_parser("set", help="persist the active mode into .reflect/config.json")
    sp.add_argument("mode_id")

    shp = sub.add_parser("show", help="print the merged mode object as JSON")
    shp.add_argument("mode_id", nargs="?", default=None)

    dp = sub.add_parser("drain-prompt", help="render the writer prompt for a queue entry")
    dp.add_argument("--target", required=True, help="transcript / slice path to reflect on")
    dp.add_argument("--trigger", default="unknown")
    dp.add_argument("--skill-name", default="")
    dp.add_argument("--transcript", default="")
    dp.add_argument("--learning-id", default="")
    dp.add_argument("--reason", default="")

    args = ap.parse_args()

    try:
        if args.cmd == "list":
            for mode_id in list_modes():
                print(mode_id)
        elif args.cmd == "get":
            print(resolve_active_mode_id())
        elif args.cmd == "set":
            path = set_active_mode(args.mode_id)
            print(f"mode={args.mode_id} -> {path}")
        elif args.cmd == "show":
            mode = load_mode(args.mode_id) if args.mode_id else get_active_mode()
            print(json.dumps(mode, indent=2, ensure_ascii=False))
        elif args.cmd == "drain-prompt":
            print(
                drain_prompt(
                    target=args.target,
                    trigger=args.trigger,
                    skill_name=args.skill_name,
                    transcript=args.transcript,
                    learning_id=args.learning_id,
                    reason=args.reason,
                ),
                end="",
            )
    except (ModeError, OSError, json.JSONDecodeError) as exc:
        print(f"mode_loader: {exc}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
