#!/usr/bin/env python3
"""Build the prove-fullstack status explainer from REPORT.md + git log.

Reads docs/hangar/proofs/fullstack/REPORT.md (leg table, defect table, run notes)
and the branch's fix/feat commits, and renders ONE self-contained HTML page on
the explain-to-me `11-status-report` template shape. Recordings are linked from
the public branch on GitHub rather than embedded, so the page stays small.

Usage: python3 scripts/hangar/build-fullstack-explainer.py [out.html]
Re-run every loop iteration, then publish with the explain-to-me publish script.
"""
from __future__ import annotations

import datetime as dt
import html
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
REPORT = ROOT / "docs/hangar/proofs/fullstack/REPORT.md"
TEMPLATE = Path.home() / ".claude/skills/explain-to-me/assets/templates/11-status-report.html"
OUT = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "explainers/prove-hangar-fullstack.html"
BRANCH = "f/prove-hangar"
RAW = f"https://raw.githubusercontent.com/stevengonsalvez/agents-in-a-box/{BRANCH}/docs/hangar/proofs/fullstack"
PR = "https://github.com/stevengonsalvez/agents-in-a-box/pull/815"
COMMIT = "https://github.com/stevengonsalvez/agents-in-a-box/commit"


def md_table(section_text: str) -> list[list[str]]:
    rows = []
    for line in section_text.splitlines():
        if not line.startswith("|") or set(line.strip()) <= {"|", "-", " "}:
            continue
        cells = [c.strip() for c in line.strip().strip("|").split("|")]
        rows.append(cells)
    return rows[1:] if rows else []


def inline_md(s: str) -> str:
    """Escape, then re-enable `code`, **bold**, and [text](url)."""
    s = html.escape(s.replace("\\|", "|"))
    s = re.sub(r"`([^`]+)`", r"<code>\1</code>", s)
    s = re.sub(r"\*\*([^*]+)\*\*", r"<strong>\1</strong>", s)
    s = re.sub(r"\[([^\]]+)\]\(([^)]+)\)", r'<a href="\2">\1</a>', s)
    s = re.sub(r"\b([0-9a-f]{8})\b", rf'<a class="pr-link" href="{COMMIT}/\1">\1</a>', s)
    return s


def section(md: str, heading: str) -> str:
    m = re.search(rf"^## {re.escape(heading)}\n(.*?)(?=^## |\Z)", md, re.S | re.M)
    return m.group(1) if m else ""


def state_class(state: str) -> str:
    s = state.lower()
    if s.startswith("pass"):
        return "low"
    if s.startswith("partial") or "in progress" in s:
        return "med"
    return "high"


def main() -> None:
    md = REPORT.read_text()
    legs = md_table(section(md, "Leg table"))
    defects = md_table(section(md, "Defects on the driven path"))
    notes = [l[2:] for l in section(md, "Run notes").splitlines() if l.startswith("- ")]
    env = md_table(section(md, "Environment disclosure"))
    commits = subprocess.run(
        ["git", "log", "--format=%h%x09%s", "origin/main..HEAD"],
        cwd=ROOT, capture_output=True, text=True, check=True,
    ).stdout.splitlines()
    fixes = [c.split("\t", 1) for c in commits if re.match(r"^[0-9a-f]+\t(fix|feat|test)\(", c)]
    total_commits = len(commits)

    green = sum(1 for l in legs if l[1].lower().startswith("pass"))
    fixed = sum(1 for d in defects if d[3].upper().startswith("FIXED"))
    now = dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%d %H:%M UTC")

    recordings = []
    for slug, title, stills in (
        ("p1-happy-path", "P1 · single-issue happy path", range(1, 7)),
        ("p2-pipeline", "P2 · role-gated pipeline, Triage → Done", range(1, 10)),
        ("p3-human-loop", "P3 · live AskUserQuestion answered from Control Center", range(1, 8)),
    ):
        gif = ROOT / f"docs/hangar/proofs/fullstack/{slug}.gif"
        if not gif.exists():
            continue
        pngs = [p for p in sorted((ROOT / "docs/hangar/proofs/fullstack").glob(f"{slug.split('-')[0]}-[0-9]-*.png"))]
        recordings.append((slug, title, pngs))

    tpl = TEMPLATE.read_text()
    head = tpl.split("<body>")[0]
    head = head.replace("<title>", "<title>prove-hangar · live proving run", 1) if "<title>" in head and "prove-hangar" not in head else head
    head = re.sub(r"<title>.*?</title>", "<title>prove-hangar · live proving run</title>", head, flags=re.S)
    extra_css = """
<style>
  .legs td:nth-child(3) { font-size: 12.5px; line-height: 1.45; }
  .rec { margin: 18px 0 28px; }
  .rec img.gif { width: 100%; border-radius: 10px; border: 1px solid #E3DACC; }
  .stills { display: grid; grid-template-columns: repeat(3, 1fr); gap: 8px; margin-top: 8px; }
  .stills a img { width: 100%; border-radius: 6px; border: 1px solid #E3DACC; }
  .ledger td:nth-child(2) { font-size: 12.5px; line-height: 1.45; }
  .tag { display:inline-block; font-size: 11px; padding: 2px 8px; border-radius: 999px; background:#E3DACC; color:#141413; }
  .tag.fixed { background:#788C5D; color:#FAF9F5; }
  .tag.queued { background:#E3DACC; }
  .tag.investigating { background:#D97757; color:#FAF9F5; }
  .env td:first-child { white-space: nowrap; color:#87867F; }
  .notes li { margin: 6px 0; font-size: 13.5px; }
</style>
"""
    body = [f"""
<body>
  <div class="page">
    <header>
      <div class="header-top">
        <h1>prove-hangar &mdash; live proving run</h1>
        <span class="auto-pill">auto-generated · refreshed every loop</span>
      </div>
      <div class="date-range">
        {now} &nbsp;&middot;&nbsp; <span class="repo">agents-in-a-box @ {BRANCH}</span>
        &nbsp;&middot;&nbsp; <a class="pr-link" href="{PR}">PR #815</a>
        &nbsp;&middot;&nbsp; {total_commits} commits on the branch
      </div>
    </header>

    <section>
      <div class="summary-band">
        <div class="stat-card"><div class="stat-num">{green}/{len(legs)}</div><div class="stat-label">legs green</div><div class="stat-delta flat">TUI-driven, real agents</div></div>
        <div class="stat-card"><div class="stat-num">{len(recordings)}</div><div class="stat-label">recordings</div><div class="stat-delta flat">green runs only</div></div>
        <div class="stat-card warn"><div class="stat-num">{len(defects)}</div><div class="stat-label">defects found</div><div class="stat-delta flat">on the driven path</div></div>
        <div class="stat-card"><div class="stat-num">{fixed}</div><div class="stat-label">fixed in this PR</div><div class="stat-delta up">{len(fixes)} fix/feat/test commits</div></div>
      </div>
    </section>

    <section>
      <h2>Legs</h2>
      <hr class="rule">
      <table class="shipped legs">
        <thead><tr><th style="width:190px">leg</th><th style="width:110px">state</th><th>evidence</th><th style="width:170px">recording</th></tr></thead>
        <tbody>
"""]
    for leg in legs:
        name, state, evidence, rec = (leg + ["", "", "", ""])[:4]
        body.append(
            f'<tr><td><strong>{inline_md(name)}</strong></td>'
            f'<td><span class="risk"><span class="risk-dot {state_class(state)}"></span>{inline_md(state)}</span></td>'
            f'<td>{inline_md(evidence)}</td><td>{inline_md(rec)}</td></tr>\n'
        )
    body.append("</tbody></table></section>\n")

    body.append('<section><h2>Recordings</h2><hr class="rule">\n')
    if not recordings:
        body.append("<p>No green recording yet.</p>")
    for slug, title, pngs in recordings:
        body.append(f'<div class="rec"><h3>{html.escape(title)}</h3>'
                    f'<a href="{RAW}/{slug}.mp4"><img class="gif" src="{RAW}/{slug}.gif" alt="{html.escape(title)}"></a>'
                    f'<div class="chart-caption">gif linked from the branch; <a href="{RAW}/{slug}.mp4">mp4</a> · tape: <code>docs/hangar/proofs/fullstack/{slug}.tape</code></div>')
        if pngs:
            body.append('<div class="stills">')
            for p in pngs:
                body.append(f'<a href="{RAW}/{p.name}"><img src="{RAW}/{p.name}" alt="{p.name}" loading="lazy"></a>')
            body.append("</div>")
        body.append("</div>\n")
    body.append("</section>\n")

    body.append('<section><h2>Defect ledger</h2><hr class="rule"><table class="shipped ledger">'
                '<thead><tr><th style="width:36px">#</th><th>defect</th><th style="width:150px">class</th><th style="width:260px">state</th></tr></thead><tbody>\n')
    for d in defects:
        num, desc, cls, state = (d + ["", "", "", ""])[:4]
        tag = "fixed" if state.upper().startswith("FIXED") else ("investigating" if "investigat" in state.lower() else "queued")
        body.append(f'<tr><td>{html.escape(num)}</td><td>{inline_md(desc)}</td><td>{inline_md(cls)}</td>'
                    f'<td><span class="tag {tag}">{tag}</span> {inline_md(state)}</td></tr>\n')
    body.append("</tbody></table></section>\n")

    body.append('<section><h2>Fix commit index</h2><hr class="rule"><table class="shipped">'
                '<thead><tr><th style="width:110px">commit</th><th>subject</th></tr></thead><tbody>\n')
    for sha, subj in fixes:
        body.append(f'<tr><td><a class="pr-link" href="{COMMIT}/{sha}">{sha}</a></td><td>{html.escape(subj)}</td></tr>\n')
    body.append("</tbody></table></section>\n")

    body.append('<section><h2>Environment disclosure</h2><hr class="rule"><table class="shipped env"><tbody>\n')
    for row in env:
        if len(row) >= 2:
            body.append(f"<tr><td>{inline_md(row[0])}</td><td>{inline_md(row[1])}</td></tr>\n")
    body.append("</tbody></table></section>\n")

    if notes:
        body.append('<section><h2>Run notes</h2><hr class="rule"><ul class="notes">\n')
        body.extend(f"<li>{inline_md(n)}</li>\n" for n in notes)
        body.append("</ul></section>\n")

    body.append(f"""
    <footer>
      Sources: <code>docs/hangar/proofs/fullstack/REPORT.md</code> &middot; <code>git log origin/main..{BRANCH}</code>
      &nbsp;&mdash;&nbsp; generated {now}
    </footer>
  </div>
</body>
</html>
""")
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(head + extra_css + "".join(body))
    print(OUT)


if __name__ == "__main__":
    main()
