# ABOUTME: Port R6 (Hindsight) — query-time date parsing. Multi-pass regex
# ABOUTME: extraction of natural-language date phrases ("last week",
# ABOUTME: "in march", "3 days ago", "since 2026-01-01", "last sprint") into
# ABOUTME: a (start, end) range with a confidence score. Stdlib only.
"""Query-time temporal extraction for recall (port R6, Hindsight).

Parses natural-language date expressions out of a recall query into a
``TemporalRange(start, end, confidence)``. Pairs with the R5 temporal
retrieval arm — without this, only explicit ISO dates could trigger
temporal ranking.

Hindsight's ``search/temporal_extraction.py`` delegates to a
``DateparserQueryAnalyzer`` (the ``dateparser`` library plus a regex pass
for period expressions). This port is a clean-room, stdlib-only
reimplementation — plugins/reflect scripts stay dependency-light, and a
date-phrase resolver doesn't need dateparser's 200-language surface:

    Pass 1: explicit ranges      — "between A and B", "from A to B"
    Pass 2: relative days        — yesterday/today, "N days ago",
                                   "a couple/few days|weeks|months ago"
    Pass 3: calendar periods     — last/this week|month|year, last weekend,
                                   "last <weekday>", "last/past N days"
    Pass 4: explicit ISO dates   — 2026-06-01, 2026/06/01
    Pass 5: months & years       — "march 2024", "in march", "in 2024"
    Pass 6: codebase phrases     — "last sprint", "this sprint", "recently"
    Modifiers (applied on top)   — before/until/since/after <anchor>

Codebase-event phrases WITHOUT a date anchor ("before the rewrite",
"before X commit") return None cleanly — there is no commit-date resolver
here; the bead's A2 (bitemporal graph edges) is the future home for that.

Contract: ``extract_temporal_constraint(query, reference_date) ->
TemporalRange | None``. Never raises — a parser bug must never break the
recall path (silent-fail discipline; Hindsight wraps dateparser the same
way).
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from datetime import datetime, timedelta
from typing import Any

# Open-ended "before X" ranges need a finite floor; epoch predates any
# learning the KB could hold.
DISTANT_PAST = datetime(1970, 1, 1)

# One agile sprint ≈ 2 weeks. "last sprint" / "this sprint" are fuzzy
# (no sprint calendar is available), hence the low confidence below.
SPRINT_DAYS = 14

_MONTHS = {
    "january": 1, "jan": 1,
    "february": 2, "feb": 2,
    "march": 3, "mar": 3,
    "april": 4, "apr": 4,
    "may": 5,
    "june": 6, "jun": 6,
    "july": 7, "jul": 7,
    "august": 8, "aug": 8,
    "september": 9, "sep": 9, "sept": 9,
    "october": 10, "oct": 10,
    "november": 11, "nov": 11,
    "december": 12, "dec": 12,
}
_MONTH_ALT = "|".join(sorted(_MONTHS, key=len, reverse=True))

_WEEKDAYS = {
    "monday": 0, "tuesday": 1, "wednesday": 2, "thursday": 3,
    "friday": 4, "saturday": 5, "sunday": 6,
}
_WEEKDAY_ALT = "|".join(_WEEKDAYS)

_NUMBER_WORDS = {
    "one": 1, "two": 2, "three": 3, "four": 4, "five": 5,
    "six": 6, "seven": 7, "eight": 8, "nine": 9, "ten": 10,
    "eleven": 11, "twelve": 12,
}
_NUM = r"(\d{1,3}|" + "|".join(_NUMBER_WORDS) + r")"

_ISO = r"(\d{4})[-/](\d{1,2})[-/](\d{1,2})"

# Bare month names ("in march") need a preposition context so modal "may"
# and verb "march" don't false-positive; month+year never does.
_MONTH_PREPOSITIONS = r"(?:in|during|around|last|since|before|after|until|from|by)"

# Anchor-prefix modifiers reshape the matched range into an open interval.
_MODIFIER_RE = re.compile(
    r"\b(before|until|till|since|after)\s+(?:the\s+)?$"
)


@dataclass(frozen=True)
class TemporalRange:
    """A resolved time range: ``[start, end]`` inclusive, with confidence.

    ``confidence`` ∈ (0, 1]: 1.0 explicit ISO dates, ~0.9 unambiguous
    calendar phrases, ~0.6 fuzzy phrases ("a few days ago", bare month),
    ~0.4-0.5 codebase-flavoured guesses ("last sprint", "recently").
    """

    start: datetime
    end: datetime
    confidence: float
    matched_text: str = ""

    def to_dict(self) -> dict[str, Any]:
        """JSON-friendly shape for render_json / downstream arms."""
        return {
            "start": self.start.isoformat(),
            "end": self.end.isoformat(),
            "confidence": round(self.confidence, 2),
            "matched_text": self.matched_text,
        }


# --- day-boundary helpers --------------------------------------------------

def _day_start(dt: datetime) -> datetime:
    return dt.replace(hour=0, minute=0, second=0, microsecond=0)


def _day_end(dt: datetime) -> datetime:
    return dt.replace(hour=23, minute=59, second=59, microsecond=999999)


def _range(
    start: datetime, end: datetime, confidence: float, text: str
) -> TemporalRange:
    """Day-aligned TemporalRange (Hindsight clamps to day bounds too)."""
    return TemporalRange(
        start=_day_start(start),
        end=_day_end(end),
        confidence=confidence,
        matched_text=text,
    )


def _month_range(year: int, month: int, confidence: float, text: str) -> TemporalRange:
    start = datetime(year, month, 1)
    if month == 12:
        end = datetime(year, 12, 31)
    else:
        end = datetime(year, month + 1, 1) - timedelta(days=1)
    return _range(start, end, confidence, text)


def _year_range(year: int, confidence: float, text: str) -> TemporalRange:
    return _range(datetime(year, 1, 1), datetime(year, 12, 31), confidence, text)


def _to_int(token: str) -> int:
    token = token.lower()
    if token in _NUMBER_WORDS:
        return _NUMBER_WORDS[token]
    return int(token)


def _safe_date(year: int, month: int, day: int) -> datetime | None:
    try:
        return datetime(year, month, day)
    except ValueError:
        return None


# --- passes ------------------------------------------------------------------
# Each pass returns (TemporalRange, (span_start, span_end)) or None. The span
# is the anchor's position in the query so the modifier pass can inspect the
# text immediately before it.

_Anchored = tuple[TemporalRange, tuple[int, int]]


def _match_explicit_range(q: str, ref: datetime) -> _Anchored | None:
    """Pass 1: "between A and B" / "from A to B" / "A to B" (ISO dates)."""
    pat = re.compile(
        r"\b(?:(?:between|from)\s+)?" + _ISO +
        r"\s+(?:and|to|until|through)\s+" + _ISO + r"\b"
    )
    for m in pat.finditer(q):
        a = _safe_date(int(m.group(1)), int(m.group(2)), int(m.group(3)))
        b = _safe_date(int(m.group(4)), int(m.group(5)), int(m.group(6)))
        if a is None or b is None:
            continue
        if b < a:
            a, b = b, a
        return _range(a, b, 1.0, m.group(0)), m.span()
    return None


def _match_relative_day(q: str, ref: datetime) -> _Anchored | None:
    """Pass 2: yesterday/today, "N days ago", couple/few fuzzies."""
    m = re.search(r"\byesterday\b", q)
    if m:
        d = ref - timedelta(days=1)
        return _range(d, d, 0.9, m.group(0)), m.span()
    m = re.search(r"\btoday\b", q)
    if m:
        return _range(ref, ref, 0.9, m.group(0)), m.span()

    # Imprecise quantities get a range, not a day (Hindsight shape:
    # couple ≈ 2 → window 1-3 units back; few ≈ 3-4 → window 2-5 back).
    fuzzy = re.search(
        r"\b(?:a\s+)?(couple|few)\s+(?:of\s+)?(day|week|month)s?\s+ago\b", q
    )
    if fuzzy:
        lo, hi = (1, 3) if fuzzy.group(1) == "couple" else (2, 5)
        unit_days = {"day": 1, "week": 7, "month": 30}[fuzzy.group(2)]
        return (
            _range(
                ref - timedelta(days=hi * unit_days),
                ref - timedelta(days=lo * unit_days),
                0.6,
                fuzzy.group(0),
            ),
            fuzzy.span(),
        )

    m = re.search(_NUM + r"\s+(day|week|month|year)s?\s+ago\b", q)
    if m:
        n = _to_int(m.group(1))
        unit = m.group(2)
        if unit == "day":
            d = ref - timedelta(days=n)
            return _range(d, d, 0.8, m.group(0)), m.span()
        if unit == "week":
            d = ref - timedelta(weeks=n)
            return _range(d, d + timedelta(days=6), 0.7, m.group(0)), m.span()
        if unit == "month":
            d = ref - timedelta(days=30 * n)
            return _range(d, d + timedelta(days=29), 0.6, m.group(0)), m.span()
        return _year_range(ref.year - n, 0.7, m.group(0)), m.span()
    return None


def _match_period(q: str, ref: datetime) -> _Anchored | None:
    """Pass 3: calendar periods relative to the reference date."""
    m = re.search(r"\b(?:last|past|previous)\s+" + _NUM +
                  r"\s+(day|week|month)s?\b", q)
    if m:
        n = _to_int(m.group(1))
        unit_days = {"day": 1, "week": 7, "month": 30}[m.group(2)]
        start = ref - timedelta(days=n * unit_days)
        return _range(start, ref, 0.8, m.group(0)), m.span()

    m = re.search(r"\blast\s+week\b", q)
    if m:
        start = ref - timedelta(days=ref.weekday() + 7)
        return _range(start, start + timedelta(days=6), 0.9, m.group(0)), m.span()
    m = re.search(r"\bthis\s+week\b", q)
    if m:
        start = ref - timedelta(days=ref.weekday())
        return _range(start, ref, 0.9, m.group(0)), m.span()

    m = re.search(r"\blast\s+month\b", q)
    if m:
        first = ref.replace(day=1)
        end = first - timedelta(days=1)
        return _range(end.replace(day=1), end, 0.9, m.group(0)), m.span()
    m = re.search(r"\bthis\s+month\b", q)
    if m:
        return _range(ref.replace(day=1), ref, 0.9, m.group(0)), m.span()

    m = re.search(r"\blast\s+year\b", q)
    if m:
        return _year_range(ref.year - 1, 0.9, m.group(0)), m.span()
    m = re.search(r"\bthis\s+year\b", q)
    if m:
        return _range(datetime(ref.year, 1, 1), ref, 0.9, m.group(0)), m.span()

    m = re.search(r"\blast\s+weekend\b", q)
    if m:
        days_since_sat = (ref.weekday() + 2) % 7 or 7
        sat = ref - timedelta(days=days_since_sat)
        return _range(sat, sat + timedelta(days=1), 0.9, m.group(0)), m.span()

    m = re.search(r"\blast\s+(" + _WEEKDAY_ALT + r")\b", q)
    if m:
        days_ago = (ref.weekday() - _WEEKDAYS[m.group(1)]) % 7 or 7
        d = ref - timedelta(days=days_ago)
        return _range(d, d, 0.9, m.group(0)), m.span()
    return None


def _match_iso_date(q: str, ref: datetime) -> _Anchored | None:
    """Pass 4: a single explicit ISO date (also YYYY/MM/DD)."""
    for m in re.finditer(r"\b" + _ISO + r"\b", q):
        d = _safe_date(int(m.group(1)), int(m.group(2)), int(m.group(3)))
        if d is None:
            continue  # 1234-56-78 lookalikes — keep scanning
        return _range(d, d, 1.0, m.group(0)), m.span()
    return None


def _match_month_year(q: str, ref: datetime) -> _Anchored | None:
    """Pass 5: "march 2024", "in march" (preposition required), "in 2024"."""
    m = re.search(r"\b(" + _MONTH_ALT + r")\.?,?\s+((?:19|20)\d{2})\b", q)
    if m:
        return (
            _month_range(int(m.group(2)), _MONTHS[m.group(1)], 0.9, m.group(0)),
            m.span(),
        )

    # Bare month: needs a preposition so "may"/"march" verbs don't trigger.
    # The anchor span is the MONTH token only — "since march" leaves "since"
    # visible to the modifier pass.
    m = re.search(
        r"\b" + _MONTH_PREPOSITIONS + r"\s+(" + _MONTH_ALT + r")\b(?!\s+(?:19|20)\d{2})",
        q,
    )
    if m:
        month = _MONTHS[m.group(1)]
        year = ref.year if month <= ref.month else ref.year - 1
        return _month_range(year, month, 0.6, m.group(1)), m.span(1)

    m = re.search(
        r"\b" + _MONTH_PREPOSITIONS + r"\s+((?:19|20)\d{2})\b(?![-/\d])", q
    )
    if m:
        return _year_range(int(m.group(1)), 0.7, m.group(1)), m.span(1)
    return None


def _match_codebase(q: str, ref: datetime) -> _Anchored | None:
    """Pass 6: codebase-flavoured phrases without a real calendar anchor."""
    m = re.search(r"\b(?:last|previous)\s+sprint\b", q)
    if m:
        return (
            _range(
                ref - timedelta(days=2 * SPRINT_DAYS),
                ref - timedelta(days=SPRINT_DAYS),
                0.5,
                m.group(0),
            ),
            m.span(),
        )
    m = re.search(r"\b(?:this|current)\s+sprint\b", q)
    if m:
        return _range(ref - timedelta(days=SPRINT_DAYS), ref, 0.5, m.group(0)), m.span()
    m = re.search(r"\b(?:recently|lately)\b", q)
    if m:
        return _range(ref - timedelta(days=14), ref, 0.4, m.group(0)), m.span()
    return None


_PASSES = (
    _match_explicit_range,
    _match_relative_day,
    _match_period,
    _match_iso_date,
    _match_month_year,
    _match_codebase,
)


def _apply_modifier(
    q: str, anchored: _Anchored, ref: datetime
) -> TemporalRange:
    """Reshape an anchor into an open range when preceded by before/since/...

    before X  → [DISTANT_PAST, last instant before X starts]
    until X   → [DISTANT_PAST, X end]
    since X   → [X start, reference day end]
    after X   → [first instant after X ends, reference day end]

    A modifier producing an inverted range (e.g. "since <future>") falls
    back to the plain anchor rather than emitting nonsense.
    """
    rng, (span_start, _) = anchored
    m = _MODIFIER_RE.search(q[:span_start])
    if not m:
        return rng
    word = m.group(1)
    text = f"{word} {rng.matched_text}"
    if word == "before":
        new = TemporalRange(
            DISTANT_PAST, rng.start - timedelta(microseconds=1),
            rng.confidence, text,
        )
    elif word in ("until", "till"):
        new = TemporalRange(DISTANT_PAST, rng.end, rng.confidence, text)
    elif word == "since":
        new = TemporalRange(rng.start, _day_end(ref), rng.confidence, text)
    else:  # after
        new = TemporalRange(
            rng.end + timedelta(microseconds=1), _day_end(ref),
            rng.confidence, text,
        )
    if new.start > new.end:
        return rng
    return new


def extract_temporal_constraint(
    query: str, reference_date: datetime | None = None
) -> TemporalRange | None:
    """Parse a natural-language date phrase out of ``query``.

    Returns a day-aligned ``TemporalRange`` (start ≤ end, confidence in
    (0, 1]) or None when the query carries no resolvable date expression —
    including codebase-event phrases with no date anchor ("before the
    rewrite"). Never raises: any internal error degrades to None so a
    parser bug can't take down the recall path.

    ``reference_date`` anchors relative phrases (defaults to now).
    """
    if not query or not query.strip():
        return None
    ref = reference_date or datetime.now()
    try:
        q = query.lower()
        for match_pass in _PASSES:
            anchored = match_pass(q, ref)
            if anchored is not None:
                return _apply_modifier(q, anchored, ref)
    except Exception:
        return None
    return None
