"""Transcript distillation (~30x compression), ported from agent-deck's
``distill.py`` onto reflect-kb's transcript schema.

A Claude Code / Codex session transcript is a JSONL file: thousands of lines,
each a JSON event (user turn, assistant turn, tool call, tool result, plus a
lot of harness bookkeeping). Feeding the raw thing to an LLM is wasteful and
leaks far more than the dialogue actually needs. :func:`distill` walks the file
line-by-line (no LLM) and emits a compact markdown timeline that keeps only the
signal:

* user / assistant *dialogue* text,
* tool *invocations* (name + abbreviated args),
* tool *errors* (successful tool results are dropped),
* skill loads and prompt markers.

Everything else — permission-mode flips, queue operations, attachments, file
history snapshots, AI titles, PR links, raw system records, and the bulky
successful tool-result payloads — is dropped. In agent-deck this consistently
hit ~30x compression; the same holds here because the dropped categories are
where the bytes live.

The record shape is the one reflect-kb already parses in
``plugins/reflect/scripts/reflect_gate.py``: each line is an object with a
``message`` ``{role, content}`` and/or a top-level ``type``. ``content`` is
either a string or a list of typed blocks (``text`` / ``tool_use`` /
``tool_result``).
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Iterator, Optional

# Top-level record ``type`` values that carry no distillation signal. Mirrors
# agent-deck's NOISE_TYPES set, plus the reflect-specific ``queue-operation``
# the bg-drainer emits.
NOISE_TYPES: frozenset[str] = frozenset(
    {
        "permission-mode",
        "queue-operation",
        "attachment",
        "file-history-snapshot",
        "ai-title",
        "pr-link",
        "system",
    }
)

# Per-field clip lengths (chars). Tuned to keep a line scannable while still
# carrying enough to identify the event. Same intent as agent-deck.
_USER_CLIP = 1000
_ASSIST_CLIP = 500
_ERROR_CLIP = 400
_PROMPT_CLIP = 1000
_BASH_CMD_CLIP = 200
_ARGS_CLIP = 200

_SKILL_MARKER = "Base directory for this skill:"
_HEARTBEAT_PREFIXES = ("[HEARTBEAT]", "[EVENT]")


@dataclass
class DistillStats:
    """Counts emitted alongside the distilled markdown (parity with agent-deck).

    Useful both for tests and for a human eyeballing whether a transcript was
    signal-rich. ``compression`` is ``src_bytes / dst_bytes`` (>= 1.0 means we
    shrank it).
    """

    lines: int = 0
    kept_user: int = 0
    kept_assist: int = 0
    kept_tool_use: int = 0
    kept_error: int = 0
    kept_prompt: int = 0
    kept_heartbeat: int = 0
    kept_skill_load: int = 0
    dropped_noise: int = 0
    src_bytes: int = 0
    dst_bytes: int = 0
    compression: float = 0.0

    @property
    def kept_total(self) -> int:
        return (
            self.kept_user
            + self.kept_assist
            + self.kept_tool_use
            + self.kept_error
            + self.kept_prompt
            + self.kept_heartbeat
            + self.kept_skill_load
        )

    def as_dict(self) -> dict:
        d = self.__dict__.copy()
        d["kept_total"] = self.kept_total
        return d


def _clip(text: str, limit: int) -> str:
    text = text.replace("\n", " ").replace("\r", " ").strip()
    if len(text) > limit:
        return text[:limit] + "…"
    return text


def _iter_records(lines: Iterable[str]) -> Iterator[dict]:
    for line in lines:
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(obj, dict):
            yield obj


def _short_uuid(rec: dict) -> str:
    """8-char stable handle for a record, for citation chaining.

    Prefer the record's own ``uuid``; fall back to message id; else ``--------``.
    """
    for key in ("uuid", "id"):
        val = rec.get(key)
        if isinstance(val, str) and val:
            return val.replace("-", "")[:8].ljust(8, "-")
    msg = rec.get("message")
    if isinstance(msg, dict):
        mid = msg.get("id")
        if isinstance(mid, str) and mid:
            return mid.replace("-", "")[:8].ljust(8, "-")
    return "--------"


def _timestamp(rec: dict) -> str:
    ts = rec.get("timestamp") or rec.get("ts") or ""
    if isinstance(ts, (int, float)):
        return str(ts)
    return str(ts)[:19]  # trim to seconds, drop trailing millis/zone noise


def _abbreviate_tool(name: str, args) -> str:
    """One-line tool summary: name + the most identifying argument."""
    if not isinstance(args, dict):
        return name
    if name == "Bash":
        cmd = args.get("command", "")
        return f"{name} | {_clip(str(cmd), _BASH_CMD_CLIP)}"
    for key in ("file_path", "path", "notebook_path"):
        if key in args:
            extra = ""
            if "pattern" in args:
                extra = f" pattern={_clip(str(args['pattern']), 60)}"
            return f"{name} | {_clip(str(args[key]), _ARGS_CLIP)}{extra}"
    try:
        blob = json.dumps(args, default=str)
    except (TypeError, ValueError):
        blob = str(args)
    return f"{name} | {_clip(blob, _ARGS_CLIP)}"


def _emit_user(rec: dict, msg: dict, content, stats: DistillStats) -> list[str]:
    out: list[str] = []
    uid = _short_uuid(rec)
    ts = _timestamp(rec)

    # User content can be plain text OR a list including tool_result blocks
    # (Claude Code threads tool returns back through a user-role message).
    texts: list[str] = []
    tool_errors: list[str] = []
    if isinstance(content, str):
        texts.append(content)
    elif isinstance(content, list):
        for block in content:
            if not isinstance(block, dict):
                continue
            btype = block.get("type")
            if btype == "text":
                texts.append(str(block.get("text", "")))
            elif btype == "tool_result" and block.get("is_error"):
                payload = block.get("content", "")
                if isinstance(payload, list):
                    payload = " ".join(
                        str(b.get("text", "")) for b in payload if isinstance(b, dict)
                    )
                tool_errors.append(str(payload))
            # Successful tool_result blocks are intentionally dropped (noise).

    for text in texts:
        text = text.strip()
        if not text:
            continue
        if any(text.startswith(p) for p in _HEARTBEAT_PREFIXES):
            out.append(f"[{uid}] {ts} HEARTBEAT")
            stats.kept_heartbeat += 1
        elif _SKILL_MARKER in text:
            skill = text.split(_SKILL_MARKER, 1)[1].strip().splitlines()[0].strip()
            out.append(f"[{uid}] {ts} SKILL_LOAD: {_clip(skill, 120)}")
            stats.kept_skill_load += 1
        else:
            out.append(f"[{uid}] {ts} USER: {_clip(text, _USER_CLIP)}")
            stats.kept_user += 1

    for err in tool_errors:
        out.append(f"[{uid}] {ts} ERROR: {_clip(err, _ERROR_CLIP)}")
        stats.kept_error += 1

    return out


def _emit_assistant(rec: dict, msg: dict, content, stats: DistillStats) -> list[str]:
    out: list[str] = []
    uid = _short_uuid(rec)
    ts = _timestamp(rec)
    blocks = content if isinstance(content, list) else [{"type": "text", "text": content}]
    for block in blocks:
        if not isinstance(block, dict):
            continue
        btype = block.get("type")
        if btype == "tool_use":
            summary = _abbreviate_tool(str(block.get("name", "?")), block.get("input"))
            out.append(f"[{uid}] {ts} TOOL: {summary}")
            stats.kept_tool_use += 1
        elif btype == "text":
            text = str(block.get("text", "")).strip()
            if text:
                out.append(f"[{uid}] {ts} ASSIST: {_clip(text, _ASSIST_CLIP)}")
                stats.kept_assist += 1
    return out


def distill(lines: Iterable[str]) -> tuple[str, DistillStats]:
    """Distill an iterable of JSONL lines into a compact markdown timeline.

    Returns ``(markdown, stats)``. The markdown is a single ``# Distilled``
    document followed by one event per line; it is safe to feed to an analyzer
    or to print under ``--dry-run`` (after sanitization).
    """
    stats = DistillStats()
    rows: list[str] = []

    for rec in _iter_records(lines):
        stats.lines += 1
        rec_type = rec.get("type")

        if rec_type in NOISE_TYPES:
            stats.dropped_noise += 1
            continue

        if rec_type == "last-prompt":
            prompt = rec.get("prompt") or rec.get("content") or ""
            rows.append(
                f"[{_short_uuid(rec)}] {_timestamp(rec)} PROMPT: {_clip(str(prompt), _PROMPT_CLIP)}"
            )
            stats.kept_prompt += 1
            continue

        msg = rec.get("message")
        if not isinstance(msg, dict):
            stats.dropped_noise += 1
            continue
        role = msg.get("role")
        content = msg.get("content")
        if role == "user":
            rows.extend(_emit_user(rec, msg, content, stats))
        elif role == "assistant":
            rows.extend(_emit_assistant(rec, msg, content, stats))
        else:
            stats.dropped_noise += 1

    markdown = "# Distilled transcript\n\n" + "\n".join(rows) + ("\n" if rows else "")
    # ``lines`` may be a one-shot iterator, so we cannot recover src_bytes here.
    # Callers that hold the raw source (``distill_text`` / ``distill_file``)
    # fill in src_bytes + the honest compression ratio.
    stats.dst_bytes = len(markdown.encode("utf-8"))
    return markdown, stats


def distill_file(src: Path, dst: Optional[Path] = None) -> DistillStats:
    """Distill a transcript file, optionally writing the markdown to ``dst``.

    This path knows the true source size, so it fills in ``src_bytes`` and the
    ``compression`` ratio honestly.
    """
    raw = src.read_text(encoding="utf-8", errors="replace")
    markdown, stats = distill(raw.splitlines())
    stats.src_bytes = len(raw.encode("utf-8"))
    if stats.dst_bytes:
        stats.compression = round(stats.src_bytes / stats.dst_bytes, 2)
    if dst is not None:
        dst.parent.mkdir(parents=True, exist_ok=True)
        dst.write_text(markdown, encoding="utf-8")
    return stats


def distill_text(raw: str) -> tuple[str, DistillStats]:
    """Convenience wrapper that distills an in-memory transcript string and
    reports an honest compression ratio (the source bytes are known here)."""
    markdown, stats = distill(raw.splitlines())
    stats.src_bytes = len(raw.encode("utf-8"))
    if stats.dst_bytes:
        stats.compression = round(stats.src_bytes / stats.dst_bytes, 2)
    return markdown, stats
