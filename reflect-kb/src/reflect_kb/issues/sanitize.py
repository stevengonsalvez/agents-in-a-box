"""Privacy sanitizer — the gate every byte crosses before it can leave the
machine for a GitHub issue.

Ported from agent-deck's ``sanitize.py`` (regex Layer 1). This is a
publish-to-external action, so the posture is deliberately conservative:
over-redact rather than under-redact. The substitutions run in a fixed,
load-bearing order (secrets first, then coarser identifiers) so a token that
also looks like a hex blob is caught as a token, not weakened to ``<hash>``.

What it strips (in order)
-------------------------
1. **Caller-supplied maps** — exact ``key=value`` replacements for known
   business/project names the regexes can't know about.
2. **Secrets (highest priority)**
   * Anthropic keys      ``sk-ant-…``            → ``<REDACTED:anthropic_key>``
   * OpenAI keys         ``sk-proj-…`` / ``sk-…``→ ``<REDACTED:openai_key>``
   * GitHub tokens       ``gh[posru]_…``         → ``<REDACTED:github_token>``
   * AWS access keys     ``AKIA…`` (20 chars)    → ``<REDACTED:aws_key>``
   * Telegram bot tokens ``\\d{8,12}:[…]{30,}``    → ``<REDACTED:telegram_token>``
   * JWTs                ``eyJ…\\.…\\.…``           → ``<REDACTED:jwt>``
   * Slack tokens        ``xox[baprs]-…``         → ``<REDACTED:slack_token>``
   * Private keys        PEM ``-----BEGIN … KEY-----`` blocks → ``<REDACTED:private_key>``
   * GitLab PATs ``glpat-…``        → ``<REDACTED:gitlab_token>``
   * Google API keys ``AIza…``      → ``<REDACTED:google_api_key>``
   * npm tokens ``npm_…``           → ``<REDACTED:npm_token>``
   * Slack webhooks ``hooks.slack.com/services/…`` → ``<REDACTED:slack_webhook>``
   * ``Authorization: Bearer <tok>`` → ``Bearer <REDACTED:bearer_token>``
   * Generic ``KEY=value`` where KEY *contains* {token, key, secret, password,
     passwd, api_key, auth, authorization} — anywhere in the identifier, so
     ``AWS_SECRET_ACCESS_KEY``/``GITLAB_TOKEN``/``GOOGLE_API_KEY``/``NPM_TOKEN``
     all match — and value is ≥12 chars → ``KEY=<REDACTED:generic_secret>``
3. **Emails**                → ``<REDACTED:email>``
4. **IPv4 addresses**        → ``<REDACTED:ipv4>``
5. **Home paths** ``/Users/<u>`` / ``/home/<u>`` → ``/Users/<user>`` etc.;
   ``/tmp/…`` → ``/tmp/<scratch>``; ``.worktrees/<name>`` → ``.worktrees/<branch>``
6. **Full UUIDs** (8-4-4-4-12)             → ``<REDACTED:uuid>``
7. **Long hex strings** (≥20 chars)        → ``<REDACTED:hash>``

After substitution a non-mutating audit pass flags anything that still *looks*
suspicious (base64 blobs, residual long hex, env-var assignments, phone-shaped
numbers, private IP ranges) so a human reviewer — or a failing test — sees it.

The function is pure: it returns a :class:`SanitizeResult` and never writes
files or makes network calls.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from typing import Optional

# ── Layer 2: secret patterns (ordered; most specific first) ──────────────────
# Each entry is (compiled_regex, replacement, kind). ``kind`` is surfaced in
# the result's ``redactions`` tally so callers can report exactly what was
# stripped.

_SECRET_PATTERNS: list[tuple[re.Pattern[str], str, str]] = [
    (re.compile(r"sk-ant-[A-Za-z0-9_\-]{20,}"), "<REDACTED:anthropic_key>", "anthropic_key"),
    (re.compile(r"sk-proj-[A-Za-z0-9_\-]{20,}"), "<REDACTED:openai_key>", "openai_key"),
    (re.compile(r"sk-[A-Za-z0-9]{20,}"), "<REDACTED:openai_key>", "openai_key"),
    # Fine-grained GitHub PATs (github_pat_<22>_<59>) must precede the classic
    # gh[posru]_ rule — the underscore mid-body and the ``i`` after ``gh`` make
    # them invisible to that pattern, so a bare token would otherwise pass
    # through unredacted into a published issue.
    (
        re.compile(r"github_pat_[A-Za-z0-9_]{50,}"),
        "<REDACTED:github_token>",
        "github_token",
    ),
    (re.compile(r"gh[posru]_[A-Za-z0-9]{20,}"), "<REDACTED:github_token>", "github_token"),
    # GitLab personal-access / pipeline tokens (glpat-…). No generic-keyword
    # anchor, so without this prefix rule a bare token leaks into a published
    # issue unredacted.
    (re.compile(r"glpat-[A-Za-z0-9_\-]{16,}"), "<REDACTED:gitlab_token>", "gitlab_token"),
    # Google API keys (AIza…). Fixed 39-char total in practice; match ≥30 of
    # body to stay tolerant without being greedy.
    (re.compile(r"\bAIza[A-Za-z0-9_\-]{30,}\b"), "<REDACTED:google_api_key>", "google_api_key"),
    # npm automation/access tokens (npm_…).
    (re.compile(r"\bnpm_[A-Za-z0-9]{30,}\b"), "<REDACTED:npm_token>", "npm_token"),
    (re.compile(r"xox[baprs]-[A-Za-z0-9\-]{10,}"), "<REDACTED:slack_token>", "slack_token"),
    # Slack incoming-webhook URLs carry a posting credential in the path.
    (
        re.compile(r"https://hooks\.slack\.com/services/\S+"),
        "<REDACTED:slack_webhook>",
        "slack_webhook",
    ),
    (re.compile(r"\bAKIA[0-9A-Z]{16}\b"), "<REDACTED:aws_key>", "aws_key"),
    (
        re.compile(r"\b\d{8,12}:[A-Za-z0-9_\-]{30,}\b"),
        "<REDACTED:telegram_token>",
        "telegram_token",
    ),
    (
        re.compile(r"\beyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+"),
        "<REDACTED:jwt>",
        "jwt",
    ),
    (
        re.compile(
            r"-----BEGIN (?:RSA |EC |OPENSSH |DSA |PGP )?PRIVATE KEY-----"
            r".*?-----END (?:RSA |EC |OPENSSH |DSA |PGP )?PRIVATE KEY-----",
            re.DOTALL,
        ),
        "<REDACTED:private_key>",
        "private_key",
    ),
    # ``Authorization: Bearer <token>`` — strip the credential, keep the scheme
    # so the line still reads. Runs after the more specific token rules above so
    # a ``Bearer eyJ…`` JWT is already redacted by the time we get here; this
    # catches opaque bearer tokens with no recognizable prefix. Only the token
    # is replaced (group 1 — the ``Bearer `` scheme — is preserved).
    (
        re.compile(r"(\bBearer\s+)[A-Za-z0-9_\-.=+/]{12,}"),
        r"\1<REDACTED:bearer_token>",
        "bearer_token",
    ),
]

# Generic KEY=value / KEY: value secret. The keyword set and the {12,} value
# length threshold are load-bearing (mirrors agent-deck GENERIC_SECRET_RE).
# The ``(?!<REDACTED:)`` guard stops this coarse rule from re-redacting a
# placeholder a more specific earlier rule already wrote (e.g. a PEM block that
# happened to follow the literal text "key:").
# The keyword is allowed to be EMBEDDED in a longer identifier. ``_`` is a word
# char, so a strict ``\b(key)\b`` boundary fails on ``AWS_SECRET_ACCESS_KEY``,
# ``GITLAB_TOKEN``, ``GOOGLE_API_KEY``, ``NPM_TOKEN`` — the keyword sits between
# word chars, never on a boundary, so the whole assignment passes through
# unredacted. Surrounding ``\w*`` lets the keyword match anywhere inside the
# identifier; group 1 captures the FULL env-var name so it's preserved verbatim
# and only the value is stripped.
_GENERIC_SECRET_RE = re.compile(
    r"\b(\w*(?:token|key|secret|password|passwd|api[_-]?key|auth(?:orization)?)\w*)"
    r"(\s*[:=]\s*)"
    r"(['\"]?)(?!<REDACTED:)([^\s'\"]{12,})\3",
    re.IGNORECASE,
)

_EMAIL_RE = re.compile(r"\b[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}\b")
_IPV4_RE = re.compile(r"\b(?:\d{1,3}\.){3}\d{1,3}\b")
_UUID_RE = re.compile(
    r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-"
    r"[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b"
)
_LONG_HEX_RE = re.compile(r"\b[0-9a-f]{20,}\b", re.IGNORECASE)

# Home/scratch paths. ``<user>`` is the placeholder; we keep the OS-specific
# prefix (/Users vs /home) so the report still reads naturally.
_HOME_PATH_RE = re.compile(r"/(Users|home)/[^/\s]+")
_TMP_PATH_RE = re.compile(r"/tmp/[^\s'\"]+")
_VAR_PATH_RE = re.compile(r"/var/(?:folders|tmp)/[^\s'\"]+")
_WORKTREE_RE = re.compile(r"(\.?worktrees)/[^/\s'\"]+")

# ── audit (non-mutating) suspicious patterns ─────────────────────────────────
_SUSPICIOUS: list[tuple[re.Pattern[str], str]] = [
    (re.compile(r"\b[0-9a-f]{20,}\b", re.IGNORECASE), "residual_long_hex"),
    (re.compile(r"\b[A-Za-z0-9+/]{40,}={0,2}\b"), "possible_base64_blob"),
    (re.compile(r"\b\w+\s*=\s*\S{8,}"), "env_var_assignment"),
    (re.compile(r"\b\+?\d[\d\s\-().]{8,}\d\b"), "possible_phone_number"),
    (re.compile(r"\b(?:10|192\.168|172\.(?:1[6-9]|2\d|3[01]))\.\d"), "private_ip_range"),
]

# Placeholders this sanitizer itself writes, e.g. ``<REDACTED:generic_secret>``.
# The audit runs on ALREADY-redacted text, so without masking these the coarse
# suspicious patterns (env_var_assignment, possible_base64_blob, …) re-flag the
# sanitizer's OWN output — a fully-redacted ``FOO=<REDACTED:generic_secret>``
# would be reported as a suspicious env assignment, burying genuine residue. We
# blank placeholder spans (to equal-length spaces, preserving line/column
# offsets) before the audit regexes run.
_PLACEHOLDER_RE = re.compile(r"<REDACTED:[a-z_]+>")


@dataclass
class SanitizeResult:
    """Outcome of one sanitize pass.

    Attributes:
        text: the sanitized output (safe to publish).
        redactions: ``{kind: count}`` of substitutions actually applied.
        audit: list of ``{kind, line, snippet}`` for residual suspicious matches
            a human should eyeball. Non-empty ``audit`` does NOT mean unsafe —
            it means "look here"; callers decide policy.
    """

    text: str
    redactions: dict[str, int] = field(default_factory=dict)
    audit: list[dict] = field(default_factory=list)

    @property
    def total_redactions(self) -> int:
        return sum(self.redactions.values())


def _bump(tally: dict[str, int], kind: str, n: int) -> None:
    if n:
        tally[kind] = tally.get(kind, 0) + n


def sanitize(text: str, *, maps: Optional[dict[str, str]] = None) -> SanitizeResult:
    """Sanitize ``text`` for external publication.

    Args:
        text: raw content (a distilled transcript, a drafted issue body, …).
        maps: caller-supplied exact substitutions (e.g. ``{"AcmeCorp":
            "<company>"}``) applied before the regex layers.

    Returns:
        A :class:`SanitizeResult`. The function is pure.
    """
    tally: dict[str, int] = {}
    out = text

    # 1. Caller maps first — they win over everything (business/project names).
    if maps:
        for needle, replacement in maps.items():
            if not needle:
                continue
            count = out.count(needle)
            if count:
                out = out.replace(needle, replacement)
                _bump(tally, "custom_map", count)

    # 2. Secrets (highest priority, ordered most-specific first).
    for pattern, replacement, kind in _SECRET_PATTERNS:
        out, n = pattern.subn(replacement, out)
        _bump(tally, kind, n)

    def _generic(m: re.Match[str]) -> str:
        return f"{m.group(1)}{m.group(2)}<REDACTED:generic_secret>"

    out, n = _GENERIC_SECRET_RE.subn(_generic, out)
    _bump(tally, "generic_secret", n)

    # 3. Emails.
    out, n = _EMAIL_RE.subn("<REDACTED:email>", out)
    _bump(tally, "email", n)

    # 4. IPv4 (after emails so an email's domain isn't mistaken for an IP).
    out, n = _IPV4_RE.subn("<REDACTED:ipv4>", out)
    _bump(tally, "ipv4", n)

    # 5. Paths.
    out, n = _HOME_PATH_RE.subn(lambda m: f"/{m.group(1)}/<user>", out)
    _bump(tally, "home_path", n)
    out, n = _TMP_PATH_RE.subn("/tmp/<scratch>", out)
    _bump(tally, "tmp_path", n)
    out, n = _VAR_PATH_RE.subn("/var/<system>", out)
    _bump(tally, "var_path", n)
    out, n = _WORKTREE_RE.subn(lambda m: f"{m.group(1)}/<branch>", out)
    _bump(tally, "worktree_path", n)

    # 6. Full UUIDs.
    out, n = _UUID_RE.subn("<REDACTED:uuid>", out)
    _bump(tally, "uuid", n)

    # 7. Long hex (last — secrets/UUIDs already consumed the structured ones).
    out, n = _LONG_HEX_RE.subn("<REDACTED:hash>", out)
    _bump(tally, "long_hex", n)

    return SanitizeResult(text=out, redactions=tally, audit=_audit(out))


def _mask_placeholders(line: str) -> str:
    """Blank this sanitizer's own ``<REDACTED:…>`` placeholders to equal-length
    spaces, so the audit regexes scan only genuinely-unredacted residue."""
    return _PLACEHOLDER_RE.sub(lambda m: " " * (m.end() - m.start()), line)


def _audit(text: str) -> list[dict]:
    """Non-mutating scan for residual suspicious tokens, by line.

    Placeholder spans this sanitizer already wrote are masked out first, so the
    audit never re-flags its own redactions (which would drown real findings).
    """
    findings: list[dict] = []
    for lineno, raw_line in enumerate(text.splitlines(), start=1):
        line = _mask_placeholders(raw_line)
        for pattern, kind in _SUSPICIOUS:
            m = pattern.search(line)
            if m:
                snippet = m.group(0)
                findings.append(
                    {
                        "kind": kind,
                        "line": lineno,
                        "snippet": snippet[:80],
                    }
                )
    return findings
