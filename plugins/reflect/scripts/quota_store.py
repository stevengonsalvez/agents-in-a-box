#!/usr/bin/env python3
# ABOUTME: Subscription-quota store + writer abort gate for the reflect drainer (port M3, pattern from claude-mem).
# ABOUTME: Ingests rate_limit telemetry from claude -p output (429/529 stderr fallback), persists with TTL, gates drains.
"""Subscription-quota-aware writer gate for the reflect drainer (port M3).

Pattern source: claude-mem's ``RateLimitStore`` (``src/services/worker/
RateLimitStore.ts``) — the Claude Agent SDK reports the live subscription
quota state as ``system`` events with subtype ``rate_limit`` carrying a
``rate_limit_info`` payload::

    {
      "status": "allowed" | "allowed_warning" | "rejected",
      "resetsAt": <epoch ms>,
      "rateLimitType": "five_hour" | "seven_day" | "seven_day_opus"
                     | "seven_day_sonnet" | "overage",
      "utilization": 0..1,
      "overageStatus": "allowed" | "allowed_warning" | "rejected",
      "isUsingOverage": bool,
      "surpassedThreshold": <number>,
    }

Clean-room reimplementation adapted to the drain context: claude-mem keeps
the store in-memory inside a long-lived worker; the reflect drainer is a
short-lived shell script spawned per SessionStart, so the store persists to
disk (``$REFLECT_STATE_DIR/quota-state.json``, last-write-wins per
``rateLimitType`` bucket) with a TTL (``REFLECT_QUOTA_TTL_SEC``, default
3600s) so a stale snapshot can never wedge the gate shut forever —
expired buckets are dropped on read and the gate fails OPEN.

Why: reflect-kb users on Claude Max plans hit quota cliffs mid-session with
no warning — a background drain quietly burning the five-hour window starves
the interactive session. Reading the SDK's own telemetry (already present in
every ``claude -p`` invocation the drainer makes) lets the writer
self-throttle: defer the queue with reason='quota_near_limit' instead of
failing opaquely. **No additional API call is ever issued purely to check
quota** — ``ingest`` parses output the drain already produced and ``check``
reads only the disk snapshot.

Gate rules (per window, in priority order):

* ``status == "rejected"`` (or ``overageStatus == "rejected"`` on the overage
  window) — the provider already declared the bucket exhausted: abort.
* ``surpassedThreshold`` truthy and NOT ``isUsingOverage`` — the subscription
  crossed its warning threshold and the user has no overage cushion: abort.
* ``utilization >= per-window threshold`` — self-imposed headroom so the
  background writer never burns the last few percent of a window that the
  interactive session needs.

API-key auth is exempt (per-call billing means the user authorized the
spend); detected from a non-empty ``ANTHROPIC_API_KEY`` unless an explicit
``--auth-method`` is passed.

The deferred-write marker (``quota-deferred.json``) records WHY the queue was
deferred. It is purely informational — queue entries are never consumed on
defer, so they replay naturally on the next drain once the gate reopens
(fresh "allowed" snapshot, or TTL expiry). ``check`` clears the marker
whenever the gate is open, so its presence always means "currently deferred".

CLI (consumed by reflect-drain-bg.sh and reflect_cost.py):
    quota_store.py ingest [--state-dir DIR] [--stderr-file F]
                                          # claude -p result JSON on stdin
    quota_store.py check  [--state-dir DIR] [--auth-method M]
                                          # -> {"abort": bool, "reason", "window"}
    quota_store.py defer  [--state-dir DIR] --reason R [--detail D] [--window W]
    quota_store.py status [--state-dir DIR] [--json]
                                          # four window fields + gate open/closed
"""

from __future__ import annotations

import json
import os
import re
import sys
import time
from dataclasses import asdict, dataclass
from pathlib import Path

# Quota windows the SDK reports, in gate-priority order. The four *status*
# windows (five_hour, seven_day, seven_day_opus, seven_day_sonnet) are what
# `status` must always print; `overage` is the billing cushion on top.
WINDOWS = ("five_hour", "seven_day_opus", "seven_day_sonnet", "seven_day", "overage")
STATUS_WINDOWS = ("five_hour", "seven_day", "seven_day_opus", "seven_day_sonnet")

# Bucket for snapshots that carry no rateLimitType (e.g. a 429/529 synthesized
# from stderr — we don't know which window tripped, only that one did).
DEFAULT_BUCKET = "default"

STATE_FILENAME = "quota-state.json"
DEFER_FILENAME = "quota-deferred.json"

DEFAULT_TTL_SEC = 3600

# Per-window utilization ceilings for subscription users. Crossing one defers
# the queue so background memory work never starves interactive sessions.
# Overridable globally via REFLECT_QUOTA_UTIL_THRESHOLD (applies to all
# windows) for testing / tuning.
DEFAULT_THRESHOLDS = {
    "five_hour": 0.95,
    "seven_day_opus": 0.93,
    "seven_day_sonnet": 0.92,
    "seven_day": 0.93,
    "overage": 0.95,
    DEFAULT_BUCKET: 0.95,
}

# stderr fallback: when the result envelope carries no rate_limit telemetry
# but the CLI screamed about quota, synthesize a rejected default-bucket
# snapshot. 429 = rate limited, 529 = overloaded.
_STDERR_PATTERNS = (
    re.compile(r"\b429\b"),
    re.compile(r"\b529\b"),
    re.compile(r"rate[ _-]?limit", re.IGNORECASE),
    re.compile(r"overloaded", re.IGNORECASE),
    re.compile(r"quota exceeded", re.IGNORECASE),
)

# Canonical info fields (claude-mem's camelCase wire shape) <- accepted aliases.
_FIELD_ALIASES = {
    "status": ("status",),
    "resetsAt": ("resetsAt", "resets_at"),
    "rateLimitType": ("rateLimitType", "rate_limit_type"),
    "utilization": ("utilization",),
    "overageStatus": ("overageStatus", "overage_status"),
    "overageResetsAt": ("overageResetsAt", "overage_resets_at"),
    "isUsingOverage": ("isUsingOverage", "is_using_overage"),
    "surpassedThreshold": ("surpassedThreshold", "surpassed_threshold"),
}

# Keys whose dict value is treated as a rate_limit_info payload wherever it
# appears in the result JSON (the envelope shape is not pinned by the CLI).
_WRAPPER_KEYS = ("rate_limit_info", "rateLimitInfo", "rate_limit")

_MAX_SCAN_DEPTH = 6


@dataclass
class GateDecision:
    """Verdict of the quota gate: should the writer abort further LLM calls?"""

    abort: bool
    reason: str = ""
    window: str = ""


# ── Paths / env ───────────────────────────────────────────────────────────────

def state_dir(override: str = "") -> Path:
    if override:
        return Path(override).expanduser()
    return Path(os.environ.get("REFLECT_STATE_DIR", str(Path.home() / ".reflect")))


def ttl_sec() -> int:
    raw = os.environ.get("REFLECT_QUOTA_TTL_SEC", "")
    try:
        n = int(raw)
        return n if n > 0 else DEFAULT_TTL_SEC
    except ValueError:
        return DEFAULT_TTL_SEC


def thresholds() -> dict:
    """Per-window utilization ceilings, with optional global env override."""
    out = dict(DEFAULT_THRESHOLDS)
    raw = os.environ.get("REFLECT_QUOTA_UTIL_THRESHOLD", "")
    if raw:
        try:
            v = float(raw)
            if 0.0 < v <= 1.0:
                out = {k: v for k in out}
        except ValueError:
            pass
    return out


# ── State persistence (last-write-wins per bucket, TTL on read) ──────────────

def load_state(sd: Path, ttl: int | None = None, now: float | None = None) -> dict:
    """Read the persisted store, dropping buckets older than the TTL.

    Expiry on read (not write) means a stale snapshot can never hold the gate
    closed past the TTL — the gate fails open and the next real run refreshes
    the telemetry.
    """
    if ttl is None:
        ttl = ttl_sec()
    if now is None:
        now = time.time()
    path = sd / STATE_FILENAME
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError, ValueError):
        return {}
    if not isinstance(raw, dict):
        return {}
    fresh: dict = {}
    for bucket, entry in raw.items():
        if not isinstance(entry, dict):
            continue
        try:
            observed = float(entry.get("observed_at", 0) or 0)
        except (TypeError, ValueError):
            observed = 0.0
        if observed and (now - observed) <= ttl:
            fresh[str(bucket)] = entry
    return fresh


def _save_state(sd: Path, state: dict) -> None:
    sd.mkdir(parents=True, exist_ok=True)
    path = sd / STATE_FILENAME
    tmp = path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(state, indent=2) + "\n", encoding="utf-8")
    tmp.replace(path)


def ingest_infos(sd: Path, infos: list, now: float | None = None) -> int:
    """Merge rate-limit snapshots into the store. Last-write-wins per bucket."""
    if not infos:
        return 0
    if now is None:
        now = time.time()
    # Load WITHOUT TTL filtering so an old-but-unexpired-on-disk bucket isn't
    # silently dropped by a merge; expiry is applied on read in load_state.
    path = sd / STATE_FILENAME
    try:
        state = json.loads(path.read_text(encoding="utf-8"))
        if not isinstance(state, dict):
            state = {}
    except (OSError, json.JSONDecodeError, ValueError):
        state = {}
    n = 0
    for info in infos:
        if not isinstance(info, dict):
            continue
        bucket = str(info.get("rateLimitType") or DEFAULT_BUCKET)
        entry = dict(info)
        entry["observed_at"] = now
        state[bucket] = entry
        n += 1
    if n:
        _save_state(sd, state)
    return n


# ── Telemetry extraction ──────────────────────────────────────────────────────

def _normalize_info(d: dict) -> dict:
    out: dict = {}
    for canon, aliases in _FIELD_ALIASES.items():
        for a in aliases:
            if a in d:
                out[canon] = d[a]
                break
    return out


def _looks_like_info(d: dict) -> bool:
    """Heuristic: a dict is a rate_limit_info payload if it names a window,
    a surpassed threshold, or a utilization+status pair."""
    if any(k in d for k in ("rateLimitType", "rate_limit_type")):
        return True
    if any(k in d for k in ("surpassedThreshold", "surpassed_threshold")):
        return True
    return "utilization" in d and "status" in d


def extract_infos(obj: object, depth: int = 0) -> list:
    """Recursively pull every rate_limit_info payload out of a parsed JSON
    value: the SDK system-event shape ({"type":"system","subtype":"rate_limit",
    "rate_limit_info":{...}}), a wrapped key anywhere in the result envelope,
    or a bare info dict. Bounded depth; tolerant of any shape."""
    found: list = []
    if depth > _MAX_SCAN_DEPTH:
        return found
    if isinstance(obj, list):
        for item in obj:
            found.extend(extract_infos(item, depth + 1))
        return found
    if not isinstance(obj, dict):
        return found
    consumed_keys: set = set()
    for key in _WRAPPER_KEYS:
        val = obj.get(key)
        if isinstance(val, dict) and (_looks_like_info(val) or "status" in val):
            found.append(_normalize_info(val))
            consumed_keys.add(key)
    if not found and _looks_like_info(obj):
        found.append(_normalize_info(obj))
        return found
    for key, val in obj.items():
        if key in consumed_keys:
            continue
        if isinstance(val, (dict, list)):
            found.extend(extract_infos(val, depth + 1))
    return found


def parse_output(text: str) -> list:
    """Extract rate-limit snapshots from claude -p output: a single JSON
    result envelope, or stream-json (one JSON object per line)."""
    if not text or not text.strip():
        return []
    text = text.strip()
    try:
        return extract_infos(json.loads(text))
    except (json.JSONDecodeError, ValueError):
        pass
    infos: list = []
    for line in text.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            infos.extend(extract_infos(json.loads(line)))
        except (json.JSONDecodeError, ValueError):
            continue
    return infos


def parse_stderr(text: str) -> list:
    """429/529/rate-limit fallback: when the CLI errored about quota but the
    envelope carried no telemetry, synthesize a rejected default-bucket
    snapshot (window unknown). TTL expiry reopens the gate."""
    if not text:
        return []
    for pat in _STDERR_PATTERNS:
        m = pat.search(text)
        if m:
            return [{
                "status": "rejected",
                "rateLimitType": None,
                "_source": "stderr",
                "_marker": m.group(0),
            }]
    return []


# ── The gate ──────────────────────────────────────────────────────────────────

def is_api_key_auth(auth_method: str = "") -> bool:
    """API-key users pay per call — they already authorized the spend."""
    if auth_method:
        normalized = auth_method.strip().lower()
        return normalized.startswith("api key") or normalized == "api_key"
    return bool((os.environ.get("ANTHROPIC_API_KEY") or "").strip())


def should_abort(state: dict, api_key_auth: bool = False,
                 limits: dict | None = None) -> GateDecision:
    """Decide whether the writer must stop issuing LLM calls.

    Reads ONLY the in-memory snapshot — never the network. Priority per
    window: provider rejection > surpassedThreshold-without-overage >
    utilization ceiling. Unknown windows / empty state = gate open.
    """
    if api_key_auth:
        return GateDecision(abort=False, reason="api_key auth: per-call billing")
    if limits is None:
        limits = thresholds()

    for window in WINDOWS + (DEFAULT_BUCKET,):
        entry = state.get(window)
        if not isinstance(entry, dict):
            continue

        rejected = entry.get("status") == "rejected" or (
            window == "overage" and entry.get("overageStatus") == "rejected"
        )
        if rejected:
            return GateDecision(
                abort=True, window=window,
                reason=f"quota:{window} rejected by provider",
            )

        # The acceptance rule: the subscription crossed its warning threshold
        # and overage isn't absorbing the spill -> stop before the hard wall.
        if entry.get("surpassedThreshold") and not entry.get("isUsingOverage"):
            return GateDecision(
                abort=True, window=window,
                reason=f"quota:{window} surpassedThreshold without overage",
            )

        util = entry.get("utilization")
        limit = limits.get(window, limits.get(DEFAULT_BUCKET, 0.95))
        if isinstance(util, (int, float)) and util >= limit:
            return GateDecision(
                abort=True, window=window,
                reason=(f"quota:{window} utilization {util * 100:.1f}% "
                        f">= {limit * 100:.0f}%"),
            )

    return GateDecision(abort=False)


# ── Deferred-write marker ─────────────────────────────────────────────────────

def write_defer_marker(sd: Path, reason: str, detail: str = "",
                       window: str = "", now: float | None = None) -> dict:
    """Record that the queue was deferred. Queue entries are NOT consumed on
    defer — they replay on the next drain once the gate reopens; this marker
    just makes the deferral observable (status / reflect:cost)."""
    if now is None:
        now = time.time()
    marker = {
        "ts": now,
        "reason": reason or "quota_near_limit",
        "detail": detail,
        "window": window,
    }
    sd.mkdir(parents=True, exist_ok=True)
    path = sd / DEFER_FILENAME
    tmp = path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(marker, indent=2) + "\n", encoding="utf-8")
    tmp.replace(path)
    return marker


def read_defer_marker(sd: Path) -> dict | None:
    try:
        raw = json.loads((sd / DEFER_FILENAME).read_text(encoding="utf-8"))
        return raw if isinstance(raw, dict) else None
    except (OSError, json.JSONDecodeError, ValueError):
        return None


def clear_defer_marker(sd: Path) -> None:
    try:
        (sd / DEFER_FILENAME).unlink()
    except OSError:
        pass


# ── Status rendering ──────────────────────────────────────────────────────────

def _fmt_entry(entry: dict | None) -> str:
    if not isinstance(entry, dict):
        return "(no data)"
    parts = []
    status = entry.get("status")
    if status:
        parts.append(str(status))
    util = entry.get("utilization")
    if isinstance(util, (int, float)):
        parts.append(f"util={util * 100:.1f}%")
    if entry.get("surpassedThreshold"):
        parts.append(f"surpassedThreshold={entry['surpassedThreshold']}")
    parts.append(f"isUsingOverage={bool(entry.get('isUsingOverage'))}")
    resets = entry.get("resetsAt")
    if isinstance(resets, (int, float)) and resets > 0:
        # SDK reports epoch ms.
        secs = resets / 1000 if resets > 4_000_000_000 else resets
        mins = max(0, (secs - time.time()) / 60)
        parts.append(f"resets in {mins:.0f}m")
    return " ".join(parts)


def status_payload(sd: Path) -> dict:
    """Machine-readable status: the four window fields, the gate verdict,
    and the deferral marker (if any)."""
    state = load_state(sd)
    decision = should_abort(state, api_key_auth=is_api_key_auth())
    return {
        "windows": {w: state.get(w) for w in STATUS_WINDOWS},
        "overage": state.get("overage"),
        "default": state.get(DEFAULT_BUCKET),
        "gate": "closed" if decision.abort else "open",
        "reason": decision.reason,
        "window": decision.window,
        "deferred": read_defer_marker(sd),
        "ttl_sec": ttl_sec(),
    }


def render_status(sd: Path) -> str:
    """Human-readable quota status for `quota_store.py status` and the
    /reflect:cost surface."""
    p = status_payload(sd)
    gate = "OPEN" if p["gate"] == "open" else "CLOSED"
    lines = [f"quota gate: {gate}" + (f" ({p['reason']})" if p["reason"] else "")]
    for w in STATUS_WINDOWS:
        lines.append(f"  {w:<17}: {_fmt_entry(p['windows'].get(w))}")
    lines.append(f"  {'overage':<17}: {_fmt_entry(p['overage'])}")
    if p["default"]:
        lines.append(f"  {'(window unknown)':<17}: {_fmt_entry(p['default'])}")
    deferred = p["deferred"]
    if deferred:
        lines.append(
            f"  deferred         : reason={deferred.get('reason', '?')}"
            + (f" detail={deferred['detail']}" if deferred.get("detail") else "")
            + " (queue entries retained; replay on next drain once the gate reopens)"
        )
    if not any(p["windows"].values()) and not p["overage"] and not p["default"]:
        lines.append(
            f"  (no quota telemetry within TTL={p['ttl_sec']}s — gate fails open;"
            " the next drain run refreshes it)"
        )
    return "\n".join(lines)


# ── CLI ───────────────────────────────────────────────────────────────────────

def main() -> None:
    import argparse

    ap = argparse.ArgumentParser(description="Subscription-quota store (port M3)")
    sub = ap.add_subparsers(dest="cmd", required=True)

    ip = sub.add_parser("ingest", help="parse claude -p output on stdin into the store")
    ip.add_argument("--state-dir", default="")
    ip.add_argument("--stderr-file", default="",
                    help="also scan this stderr capture for 429/529 markers")

    cp = sub.add_parser("check", help="print the gate verdict as JSON")
    cp.add_argument("--state-dir", default="")
    cp.add_argument("--auth-method", default="",
                    help="override auth detection (api_key = never abort)")

    dp = sub.add_parser("defer", help="record the deferred-write marker")
    dp.add_argument("--state-dir", default="")
    dp.add_argument("--reason", default="quota_near_limit")
    dp.add_argument("--detail", default="")
    dp.add_argument("--window", default="")

    sp = sub.add_parser("status", help="print quota windows + gate state")
    sp.add_argument("--state-dir", default="")
    sp.add_argument("--json", action="store_true")

    args = ap.parse_args()
    sd = state_dir(getattr(args, "state_dir", ""))

    if args.cmd == "ingest":
        stdin_text = sys.stdin.read()
        infos = parse_output(stdin_text)
        if args.stderr_file:
            try:
                stderr_text = Path(args.stderr_file).read_text(
                    encoding="utf-8", errors="replace")
            except OSError:
                stderr_text = ""
            # Envelope telemetry is authoritative; the stderr heuristic only
            # fills in when the run produced none.
            if not infos:
                infos = parse_stderr(stderr_text)
        n = ingest_infos(sd, infos)
        print(json.dumps({"ingested": n}))
    elif args.cmd == "check":
        state = load_state(sd)
        decision = should_abort(
            state, api_key_auth=is_api_key_auth(args.auth_method))
        if not decision.abort:
            # Gate open => any standing deferral is resolved; the marker's
            # presence must always mean "currently deferred".
            clear_defer_marker(sd)
        print(json.dumps(asdict(decision)))
    elif args.cmd == "defer":
        marker = write_defer_marker(sd, args.reason, args.detail, args.window)
        print(json.dumps(marker))
    elif args.cmd == "status":
        if args.json:
            print(json.dumps(status_payload(sd), indent=2))
        else:
            print(render_status(sd))


if __name__ == "__main__":
    main()
