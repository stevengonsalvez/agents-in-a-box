# ABOUTME: Load + validate the phone-bridge config from ainb's config.toml.
#
# Config lives in ainb's layered config at
#   ~/.agents-in-a-box/config/config.toml
# under a dedicated [fleet.telegram] table. The bot token and authorized user id
# are resolved through the secret resolver so the token is NEVER written on argv
# or into the launchd/systemd unit — it stays in config (or a keychain/env ref).
#
#   [fleet.telegram]
#   token = "$TELEGRAM_BOT_TOKEN"      # or "keychain:svc" or a literal
#   user_id = 123456789               # authorized Telegram user id (int or str)
#   default_target = "conductor"      # optional: name to prefer when no prefix
#   require_mention_in_groups = true  # optional, default true
#   response_timeout = 300            # optional, seconds
#
# The TOML is parsed with the stdlib `tomllib` (Python 3.11+), so the bridge has
# zero parsing dependencies.

from __future__ import annotations

import os
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

from . import RESPONSE_TIMEOUT
from .secrets import resolve_secret


class ConfigError(RuntimeError):
    """Raised when the bridge config is missing or invalid."""


def default_config_path() -> Path:
    """Resolve ainb's config.toml path, honouring AINB_CONFIG_PATH override."""
    override = os.environ.get("AINB_CONFIG_PATH")
    if override:
        return Path(override).expanduser()
    return Path.home() / ".agents-in-a-box" / "config" / "config.toml"


@dataclass
class BridgeConfig:
    """Resolved, validated bridge configuration."""

    token: str
    authorized_user_id: int
    default_target: str | None = None
    require_mention_in_groups: bool = True
    response_timeout: int = RESPONSE_TIMEOUT
    proxy_url: str | None = None
    extra: dict = field(default_factory=dict)


def _coerce_user_id(value: object) -> int:
    """Accept an int or a secret-resolvable string for user_id."""
    if isinstance(value, bool):  # bool is an int subclass — reject explicitly
        raise ConfigError("user_id must be a number, not a boolean")
    if isinstance(value, int):
        return value
    if isinstance(value, str):
        resolved = resolve_secret(value).strip()
        if not resolved:
            raise ConfigError("user_id is empty after secret resolution")
        try:
            return int(resolved)
        except ValueError as exc:
            raise ConfigError(f"user_id is not an integer: {resolved!r}") from exc
    raise ConfigError(f"user_id has unsupported type {type(value).__name__}")


def parse_config(raw: dict) -> BridgeConfig:
    """Build a :class:`BridgeConfig` from a parsed TOML mapping.

    Split out from :func:`load_config` so it can be unit-tested without touching
    the filesystem.
    """
    fleet = raw.get("fleet")
    if not isinstance(fleet, dict):
        raise ConfigError("config has no [fleet] table")
    tg = fleet.get("telegram")
    if not isinstance(tg, dict):
        raise ConfigError("config has no [fleet.telegram] table")

    if "token" not in tg:
        raise ConfigError("[fleet.telegram] is missing `token`")
    if "user_id" not in tg:
        raise ConfigError("[fleet.telegram] is missing `user_id`")

    token = resolve_secret(str(tg["token"])).strip()
    if not token:
        raise ConfigError("telegram token resolved to empty — check the env var / keychain ref")

    user_id = _coerce_user_id(tg["user_id"])

    default_target = tg.get("default_target")
    if default_target is not None:
        default_target = str(default_target).strip() or None

    require_mention = bool(tg.get("require_mention_in_groups", True))

    response_timeout = tg.get("response_timeout", RESPONSE_TIMEOUT)
    try:
        response_timeout = int(response_timeout)
    except (TypeError, ValueError) as exc:
        raise ConfigError("response_timeout must be an integer") from exc
    if response_timeout <= 0:
        raise ConfigError("response_timeout must be positive")

    proxy = tg.get("proxy_url")
    proxy = str(proxy).strip() if proxy else None

    return BridgeConfig(
        token=token,
        authorized_user_id=user_id,
        default_target=default_target,
        require_mention_in_groups=require_mention,
        response_timeout=response_timeout,
        proxy_url=proxy,
    )


def load_config(path: Path | None = None) -> BridgeConfig:
    """Load and validate the bridge config from disk."""
    cfg_path = path or default_config_path()
    if not cfg_path.exists():
        raise ConfigError(f"config file not found: {cfg_path}")
    try:
        with cfg_path.open("rb") as fh:
            raw = tomllib.load(fh)
    except (OSError, tomllib.TOMLDecodeError) as exc:
        raise ConfigError(f"failed to read {cfg_path}: {exc}") from exc
    return parse_config(raw)
