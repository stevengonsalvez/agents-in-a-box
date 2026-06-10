#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
Layered TOML Configuration for Reflect.

Load order (later wins):
  1. Plugin default  — reflect.toml next to this package
  2. User override   — ~/.reflect/reflect.toml
  3. Project override — ./.reflect.toml
  4. Environment variables (REFLECT_DB_PATH, REFLECT_PROVIDERS, etc.)

All layers are deep-merged so a project override can tweak a single key
without restating the entire config.
"""

import os
import sys
import tomllib
from pathlib import Path
from typing import Any

# ---------------------------------------------------------------------------
# Internal helpers
# ---------------------------------------------------------------------------

_CONFIG_CACHE: dict[str, Any] | None = None


def _deep_merge(base: dict, override: dict) -> dict:
    """Recursively merge *override* into a copy of *base*."""
    merged = base.copy()
    for key, value in override.items():
        if key in merged and isinstance(merged[key], dict) and isinstance(value, dict):
            merged[key] = _deep_merge(merged[key], value)
        else:
            merged[key] = value
    return merged


def _load_toml(path: Path) -> dict[str, Any]:
    """Load a TOML file. Returns empty dict when the file is missing or invalid."""
    if not path.is_file():
        return {}
    try:
        with open(path, "rb") as fh:
            return tomllib.load(fh)
    except Exception:
        return {}


def _plugin_default_path() -> Path:
    """Path to the plugin-bundled reflect.toml."""
    return Path(__file__).resolve().parent.parent / "reflect.toml"


def _user_override_path() -> Path:
    return Path.home() / ".reflect" / "reflect.toml"


def _project_override_path() -> Path:
    return Path.cwd() / ".reflect.toml"


def _apply_env_overrides(cfg: dict[str, Any]) -> dict[str, Any]:
    """Apply REFLECT_* environment variables on top of *cfg*."""
    env_map: dict[str, tuple[list[str], type]] = {
        # M4: pluggable mode — highest-priority override for the active mode
        # id (mode_loader.py also consults this before the TOML cascade).
        "REFLECT_MODE": (["mode"], str),
        "REFLECT_DB_PATH": (["storage", "db_path"], str),
        "REFLECT_ARTIFACTS_DIR": (["storage", "artifacts_dir"], str),
        "REFLECT_PROVIDERS": (["discovery", "enabled_providers"], list),
        "REFLECT_STALENESS_DAYS": (["discovery", "staleness_days"], int),
        "REFLECT_LOG_LEVEL": (["telemetry", "log_level"], str),
        "REFLECT_RETENTION_DAYS": (["policies", "retention_days"], int),
        "REFLECT_AUTO_APPROVE": (["policies", "auto_approve_threshold"], float),
        # R2: cross-encoder rerank knobs (recall.py reads the matching
        # RECALL_CROSS_ENCODER / RECALL_CE_TIMEOUT env vars directly since
        # it runs as a standalone uv script; these config keys are the
        # declarative surface for hooks/tools that wire its environment).
        "REFLECT_RECALL_CE_ENABLED": (["recall", "cross_encoder", "enabled"], bool),
        "REFLECT_RECALL_CE_MODEL": (["recall", "cross_encoder", "model"], str),
        "REFLECT_RECALL_CE_CANDIDATES": (["recall", "cross_encoder", "candidates"], int),
        "REFLECT_RECALL_CE_TIMEOUT": (["recall", "cross_encoder", "timeout_s"], int),
        # R16: project-affinity boost strength (recall.py reads the matching
        # RECALL_PROJECT_ALPHA env var directly; this key is the declarative
        # surface for hooks/tools that wire its environment). 0 disables.
        "REFLECT_RECALL_PROJECT_ALPHA": (
            ["recall", "boost", "project_affinity_alpha"], float,
        ),
    }

    for env_key, (path_keys, cast) in env_map.items():
        raw = os.environ.get(env_key)
        if raw is None:
            continue

        # Cast the raw string to the expected type
        if cast is list:
            value: Any = [s.strip() for s in raw.split(",") if s.strip()]
        elif cast is bool:
            value = raw.strip().lower() not in {"0", "false", "no", "off", ""}
        elif cast is int:
            value = int(raw)
        elif cast is float:
            value = float(raw)
        else:
            value = raw

        # Walk the nested dict and set the leaf
        node = cfg
        for k in path_keys[:-1]:
            node = node.setdefault(k, {})
        node[path_keys[-1]] = value

    return cfg


# ---------------------------------------------------------------------------
# Defaults (used when no TOML file is present at all)
# ---------------------------------------------------------------------------

_BUILTIN_DEFAULTS: dict[str, Any] = {
    # M4: active reflect mode (references/modes/{id}.json, parent--override
    # inheritance via mode_loader.py). The default `engineering` mode is the
    # historical taxonomy expressed declaratively — zero behaviour change.
    "mode": "engineering",
    "storage": {
        "db_path": "~/.reflect/reflect.db",
        "artifacts_dir": "docs/solutions",
    },
    "discovery": {
        "enabled_providers": ["claude", "codex", "gemini"],
        "staleness_days": 30,
    },
    "providers": {
        "claude": {
            "projects_dir": "~/.claude/projects",
            # Match every .md under memory/ — both the consolidated MEMORY.md
            # index AND atomic per-fact files. See reflect.toml comment.
            "memory_pattern": "*/memory/*.md",
        },
        "codex": {
            "home_dir": "~/.codex",
            "memories_dir": "~/.codex/memories",
            "agents_md": "~/.codex/AGENTS.md",
        },
        "gemini": {
            "home_dir": "~/.gemini",
            "global_md": "~/.gemini/GEMINI.md",
        },
    },
    "indexers": {
        "graphrag": {
            # Canonical CLI is `reflect` (reflect-kb). Bare name is resolved
            # through $PATH so any install location (`uv tool install reflect-kb`,
            # homebrew, etc.) works without per-machine overrides.
            "cli_path": "reflect",
            "auto_sidecar": True,
        },
    },
    "policies": {
        "auto_approve_threshold": 0.8,
        "retention_days": 90,
        "max_memory_lines": 200,
    },
    "recall": {
        # R2: cross-encoder rerank (engine-side `reflect rerank`; model
        # auto-downloads to ~/.reflect/models/ on first use).
        "cross_encoder": {
            "enabled": True,
            "model": "cross-encoder/ms-marco-MiniLM-L-6-v2",
            "candidates": 20,
            "timeout_s": 60,
        },
        # R16: bounded multiplicative boost knobs. project_affinity_alpha
        # is the R8 bounded-boost α — same-project hits get 1 + α/2 (+10%
        # at the default 0.2); cross-project hits are exactly unchanged.
        # Set to 0 to disable. Soft affinity, not hard isolation.
        "boost": {
            "project_affinity_alpha": 0.2,
        },
    },
    "telemetry": {
        "enabled": True,
        "log_level": "info",
    },
}

# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------


def load_config(*, force_reload: bool = False) -> dict[str, Any]:
    """
    Build the fully-merged config dict.

    The result is cached after the first call.  Pass *force_reload=True*
    to re-read from disk (useful in tests).
    """
    global _CONFIG_CACHE
    if _CONFIG_CACHE is not None and not force_reload:
        return _CONFIG_CACHE

    cfg = _BUILTIN_DEFAULTS.copy()
    cfg = _deep_merge(cfg, _load_toml(_plugin_default_path()))
    cfg = _deep_merge(cfg, _load_toml(_user_override_path()))
    cfg = _deep_merge(cfg, _load_toml(_project_override_path()))
    cfg = _apply_env_overrides(cfg)

    _CONFIG_CACHE = cfg
    return cfg


def get_config() -> dict[str, Any]:
    """Convenience alias — returns cached config (loads on first access)."""
    return load_config()


def resolve_path(raw: str) -> Path:
    """Expand ``~`` and environment variables in a path string."""
    return Path(os.path.expandvars(raw)).expanduser()


# ---------------------------------------------------------------------------
# CLI — handy for debugging
# ---------------------------------------------------------------------------

def main() -> None:
    import json

    cfg = load_config(force_reload=True)
    print(json.dumps(cfg, indent=2, default=str))


if __name__ == "__main__":
    main()
