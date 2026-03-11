"""
Hook Profile Gating System

Controls which hooks run based on environment variables:
- ECC_HOOK_PROFILE: minimal|standard|strict (default: standard)
- ECC_DISABLED_HOOKS: comma-separated hook IDs to disable

Usage in any hook:
    from utils.hook_profile import is_hook_enabled

    if not is_hook_enabled("my-hook-id", profiles=["standard", "strict"]):
        sys.exit(0)  # Skip this hook
"""

import os
import sys
from typing import Set, List, Optional


VALID_PROFILES = {"minimal", "standard", "strict"}


def get_hook_profile() -> str:
    """Get current hook profile from ECC_HOOK_PROFILE env var."""
    raw = os.environ.get("ECC_HOOK_PROFILE", "standard").strip().lower()
    return raw if raw in VALID_PROFILES else "standard"


def get_disabled_hooks() -> Set[str]:
    """Get set of disabled hook IDs from ECC_DISABLED_HOOKS env var."""
    raw = os.environ.get("ECC_DISABLED_HOOKS", "").strip()
    if not raw:
        return set()
    return {h.strip().lower() for h in raw.split(",") if h.strip()}


def is_hook_enabled(hook_id: str, profiles: Optional[List[str]] = None) -> bool:
    """
    Check if a hook should run based on profile and disabled list.

    Args:
        hook_id: Unique identifier for this hook (e.g., "cost-tracker", "ts-check")
        profiles: List of profiles where this hook is active.
                  Default: ["standard", "strict"]

    Returns:
        True if the hook should run, False if it should be skipped.
    """
    if not hook_id:
        return True

    normalized_id = hook_id.strip().lower()

    # Check disabled list first
    if normalized_id in get_disabled_hooks():
        return False

    # Check profile
    if profiles is None:
        profiles = ["standard", "strict"]

    allowed = [p.strip().lower() for p in profiles if p.strip().lower() in VALID_PROFILES]
    if not allowed:
        allowed = ["standard", "strict"]

    return get_hook_profile() in allowed
