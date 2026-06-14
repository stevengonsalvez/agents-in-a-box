# ABOUTME: Secret resolver for the phone bridge config.
#
# Mirrors agent-deck's embedded-template `_resolve_secret` contract (the verified
# one, NOT the older standalone bridge.py which only accepts plain strings):
#   "$ENV_VAR"     -> os.environ.get("ENV_VAR", "")  (warns if unset)
#   "${ENV_VAR}"   -> same branch (strips both '$' and surrounding '{}')
#   "keychain:svc" -> `security find-generic-password -s svc -w` (macOS only)
#   "plain-string" -> returned unchanged
#
# A secret is NEVER passed on argv into the daemon — the bridge reads the raw
# value from config and resolves it here, in-process, at startup.

from __future__ import annotations

import logging
import subprocess

log = logging.getLogger("ainb.bridge.secrets")

KEYCHAIN_PREFIX = "keychain:"
# `security` can hang on a locked keychain prompt; bound it.
_KEYCHAIN_TIMEOUT_S = 5


def resolve_secret(value: str) -> str:
    """Resolve a config secret reference to its plaintext value.

    Pure aside from reading ``os.environ`` and (for keychain refs) shelling out
    to ``/usr/bin/security``. Unknown / missing references resolve to ``""`` with
    a warning rather than raising — the bridge then fails loudly on an empty
    token at startup, which is a clearer error than a stack trace deep in here.
    """
    import os

    if not isinstance(value, str):
        return ""

    raw = value.strip()
    if not raw:
        return ""

    if raw.startswith("$"):
        # Handles both "$VAR" and "${VAR}" — strip the leading '$' and any
        # surrounding braces, then read the environment (default "").
        name = raw.lstrip("$").strip("{}")
        resolved = os.environ.get(name, "")
        if not resolved:
            log.warning("env var %s referenced by config is unset/empty", name)
        return resolved

    if raw.startswith(KEYCHAIN_PREFIX):
        service = raw[len(KEYCHAIN_PREFIX) :]
        if not service:
            log.warning("keychain reference has no service name")
            return ""
        try:
            out = subprocess.run(
                ["/usr/bin/security", "find-generic-password", "-s", service, "-w"],
                capture_output=True,
                text=True,
                timeout=_KEYCHAIN_TIMEOUT_S,
                check=False,
            )
        except (OSError, subprocess.TimeoutExpired) as exc:
            log.warning("keychain lookup for %s failed: %s", service, exc)
            return ""
        if out.returncode != 0:
            log.warning("keychain has no item for service %s", service)
            return ""
        return out.stdout.strip()

    # Plain literal — returned as-is.
    return raw
