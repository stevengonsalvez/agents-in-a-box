"""LLM analysis step: distilled transcripts → candidate GitHub issues.

This is the one place a model is needed. Per-transcript distillation
(:mod:`reflect_kb.issues.distill`) and sanitization
(:mod:`reflect_kb.issues.sanitize`) are pure Python; the analyzer reads the
distilled timelines and proposes actionable product/code findings as structured
candidate issues.

Auth gating
-----------
The analyzer shells out to ``claude -p`` (the same runtime the reflect
bg-drainer uses). If no live Claude auth context is present — or the ``claude``
binary is missing, or it errors — :func:`analyze` returns ``([], reason)``
instead of raising. The caller surfaces ``reason`` and continues; nothing is
silently dropped, and the rest of the pipeline (dry-run preview, dedupe) still
works against whatever candidates exist.

Determinism for tests
----------------------
``analyze`` takes an injectable ``runner`` (defaults to a real ``subprocess``
call). Tests pass a fake that returns canned JSON, so the analyze → sanitize →
dedupe → file path is exercised end-to-end without a model.
"""

from __future__ import annotations

import json
import shutil
import subprocess
from typing import Callable, Optional

from reflect_kb.issues.dedupe import CandidateIssue

Runner = Callable[..., subprocess.CompletedProcess]

# Surfaces an issue may belong to — constrains the LLM's label space and is the
# same taxonomy agent-deck's issue-drafter used.
_SURFACES = ("cli", "tui", "webui", "db", "runtime", "plugin", "docs")

_ANALYZER_PROMPT = """\
You are analyzing distilled timelines of past AI coding-agent sessions to find
ACTIONABLE product or code findings worth filing as GitHub issues: recurring
bugs, capability gaps, friction points, or broken workflows.

Hard rules:
- Output ONLY a JSON array. No prose, no markdown fences.
- Each element: {{"title": str (<70 chars, no trailing period),
  "labels": [str, ...] (subset of {surfaces} plus "bug" or "enhancement"),
  "body": str (markdown: ## Summary / ## Evidence / ## Expected vs actual /
  ## Severity / ## Where to look)}}.
- Only file findings backed by evidence in the timelines. If nothing is
  actionable, output [].
- DO NOT include real names, absolute home paths, IP addresses, tokens, emails,
  or any secret. Describe the shape of the problem, not private details.
- Prefer 0-5 high-signal findings over many weak ones.

Distilled timelines:
{timelines}
"""


def _default_runner(cmd: list[str], *, timeout: int = 300) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, check=True, capture_output=True, text=True, timeout=timeout)


def claude_available() -> bool:
    """Best-effort check that a usable ``claude`` runtime is on PATH."""
    return shutil.which("claude") is not None


def _coerce_candidates(payload) -> list[CandidateIssue]:
    """Turn parsed model output into CandidateIssue objects, defensively."""
    out: list[CandidateIssue] = []
    if not isinstance(payload, list):
        return out
    for item in payload:
        if not isinstance(item, dict):
            continue
        title = str(item.get("title", "")).strip()
        body = str(item.get("body", "")).strip()
        if not title or not body:
            continue
        raw_labels = item.get("labels", [])
        labels = (
            [str(label_value) for label_value in raw_labels if str(label_value).strip()]
            if isinstance(raw_labels, list)
            else []
        )
        out.append(CandidateIssue(title=title[:120], body=body, labels=labels))
    return out


def _extract_json_array(text: str) -> Optional[list]:
    """Pull the first top-level JSON array out of model stdout.

    ``claude -p --output-format json`` wraps the answer in an envelope; the
    actual array may be nested under ``result``/``content`` or printed bare.
    We try, in order: whole-text parse, common envelope keys, then a bracket
    scan for the first ``[ ... ]`` span.
    """
    text = text.strip()
    if not text:
        return None

    try:
        parsed = json.loads(text)
        if isinstance(parsed, list):
            return parsed
        if isinstance(parsed, dict):
            for key in ("result", "content", "output", "text"):
                val = parsed.get(key)
                if isinstance(val, list):
                    return val
                if isinstance(val, str):
                    inner = _extract_json_array(val)
                    if inner is not None:
                        return inner
    except json.JSONDecodeError:
        pass

    start = text.find("[")
    end = text.rfind("]")
    if start != -1 and end != -1 and end > start:
        try:
            candidate = json.loads(text[start : end + 1])
            if isinstance(candidate, list):
                return candidate
        except json.JSONDecodeError:
            return None
    return None


def analyze(
    timelines: list[str],
    *,
    model: str = "sonnet",
    runner: Optional[Runner] = None,
    require_claude: bool = True,
    timeout: int = 300,
) -> tuple[list[CandidateIssue], str]:
    """Analyze distilled ``timelines`` into candidate issues.

    Returns ``(candidates, reason)``. ``reason`` is ``"ok"`` on success, or a
    short machine-readable code on graceful degradation (``"no-timelines"``,
    ``"claude-unavailable"``, ``"claude-error:<Type>"``, ``"unparseable"``).
    """
    if not timelines:
        return [], "no-timelines"

    run = runner or _default_runner
    # Only enforce the binary check on the real path; an injected runner means
    # a test (or an alternate backend) is driving us.
    if require_claude and runner is None and not claude_available():
        return [], "claude-unavailable"

    prompt = _ANALYZER_PROMPT.format(
        surfaces=list(_SURFACES),
        timelines="\n\n---\n\n".join(timelines),
    )
    cmd = [
        "claude",
        "-p",
        prompt,
        "--model",
        model,
        "--output-format",
        "json",
        "--max-turns",
        "3",
    ]
    try:
        res = run(cmd, timeout=timeout) if runner is None else run(cmd)
    except subprocess.CalledProcessError as exc:
        return [], f"claude-error:CalledProcessError:{(exc.stderr or '')[:80]}"
    except (FileNotFoundError, OSError) as exc:
        return [], f"claude-error:{type(exc).__name__}"
    except subprocess.TimeoutExpired:
        return [], "claude-error:Timeout"

    parsed = _extract_json_array(getattr(res, "stdout", "") or "")
    if parsed is None:
        return [], "unparseable"
    return _coerce_candidates(parsed), "ok"
