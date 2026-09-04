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
BRANCH = "main"
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
    def git_log(range_spec: str) -> list[str]:
        return subprocess.run(
            ["git", "log", "--format=%h%x09%s", range_spec],
            cwd=ROOT, capture_output=True, text=True, check=True,
        ).stdout.splitlines()

    commits = git_log("origin/main..HEAD")
    if not commits:
        # Merged: the branch is an ancestor of main. Read the merge commit's
        # second-parent range so the index keeps showing the run's commits.
        merge = subprocess.run(
            ["git", "log", "--merges", "--first-parent", "-1", "--format=%H",
             "--grep", "f/prove-hangar", "origin/main"],
            cwd=ROOT, capture_output=True, text=True, check=True,
        ).stdout.strip()
        if merge:
            commits = git_log(f"{merge}^1..{merge}^2")
    fixes = [c.split("\t", 1) for c in commits if re.match(r"^[0-9a-f]+\t(fix|feat|test)\(", c)]
    total_commits = len(commits)

    green = sum(1 for l in legs if l[1].lower().startswith("pass"))
    fixed = sum(1 for d in defects if d[3].upper().startswith("FIXED"))
    now = dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%d %H:%M UTC")

    # `era` is load-bearing, not decoration: the first four legs were recorded
    # against the 16-tab UI that the renovation replaced, so an unlabelled page
    # implies they show today's product. PRE renders a warning badge ABOVE the
    # player, where it is read before anyone presses play.
    PRE = ("pre", "recorded on the pre-renovation UI (16-tab strip)")
    NEW = ("new", "renovated UI · 7-tab strip, nine screens behind the palette")
    recordings = []
    for slug, title, era in (
        ("p1-happy-path", "P1 · single-issue happy path", PRE),
        ("p2-pipeline", "P2 · role-gated pipeline, Triage → Done", PRE),
        ("p3-human-loop", "P3 · live AskUserQuestion answered from Control Center", PRE),
        ("p4-levers", "P4 · levers + observability after real runs", PRE),
        ("p3-acp-human-loop", "P3-ACP · the same loop with zero tmux, answered from the Inbox", NEW),
    ):
        gif = ROOT / f"docs/hangar/proofs/fullstack/{slug}.gif"
        if not gif.exists():
            continue
        # rsplit, not split: the ACP leg's prefix is "p3-acp", and a bare
        # split would hand it "p3" and glob the tmux leg's stills instead.
        prefix = slug.rsplit("-", 2)[0]
        pngs = sorted(
            (ROOT / "docs/hangar/proofs/fullstack").glob(f"{prefix}-[0-9]*-*.png"),
            key=lambda q: int(q.name[len(prefix) + 1:].split("-")[0]),
        )
        recordings.append((slug, title, era, pngs))

    tpl = TEMPLATE.read_text()
    head = tpl.split("<body>")[0]
    head = head.replace("<title>", "<title>prove-hangar · live proving run", 1) if "<title>" in head and "prove-hangar" not in head else head
    head = re.sub(r"<title>.*?</title>", "<title>prove-hangar · live proving run</title>", head, flags=re.S)
    extra_css = """
<style>
  .legs td:nth-child(3) { font-size: 12.5px; line-height: 1.45; }
  .rec { margin: 18px 0 28px; }
  .rec img.gif { width: 100%; border-radius: 10px; border: 1px solid #E3DACC; }
  .era { display: block; font-size: 12.5px; font-weight: 600; padding: 7px 11px;
         border-radius: 7px; margin: 6px 0 9px; border-left: 3px solid; }
  .era-pre { background: #F6E7DF; color: #8A3D1E; border-left-color: #D97757; }
  .era-new { background: #E8EDE0; color: #3F4C2C; border-left-color: #788C5D; }
  .era-banner { margin: 0 0 6px; padding: 12px 14px; border-radius: 9px;
                background: #F6E7DF; border-left: 3px solid #D97757;
                color: #5C3524; font-size: 13.5px; line-height: 1.5; }
  .era-banner strong { color: #8A3D1E; }
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
    # Name the pre-renovation legs from the data, so the banner cannot drift
    # out of step with the badges the way a hand-written sentence would.
    pre_legs = [t.split("·")[0].strip() for _, t, e, _ in recordings if e is PRE]
    new_legs = [t.split("·")[0].strip() for _, t, e, _ in recordings if e is NEW]
    era_banner = ""
    if pre_legs and new_legs:
        era_banner = f"""
    <div class="era-banner">
      <strong>Two UI eras on this page.</strong> The hangar was renovated after most of
      these legs were recorded: the tab strip went from 16 entries to 7, nine screens
      moved behind the command palette, and Kanban became Runs.
      {", ".join(html.escape(n) for n in pre_legs)} predate that work and show the old
      interface; only {", ".join(html.escape(n) for n in new_legs)} shows the current one.
      The older legs are kept because each still proves what it was recorded to prove.
    </div>
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
{era_banner}
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
    for slug, title, era, pngs in recordings:
        era_cls, era_text = era
        body.append(f'<div class="rec"><h3>{html.escape(title)}</h3>'
                    f'<div class="era era-{era_cls}">{html.escape(era_text)}</div>'
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

    ship = section(md, "Ship phase: review")
    if ship.strip():
        body.append('<section><h2>Ship phase: review</h2><hr class="rule">\n')
        paras = [l for l in ship.splitlines() if l.strip() and not l.startswith("|")]
        for para in paras:
            body.append(f"<p>{inline_md(para)}</p>\n")
        rows = md_table(ship)
        if rows:
            body.append('<table class="shipped ledger"><thead><tr><th style="width:150px">area</th>'
                        '<th>round-1 finding</th><th>fix</th></tr></thead><tbody>\n')
            for r in rows:
                area, finding, fix = (r + ["", "", ""])[:3]
                body.append(f"<tr><td>{inline_md(area)}</td><td>{inline_md(finding)}</td><td>{inline_md(fix)}</td></tr>\n")
            body.append("</tbody></table>\n")
        body.append("</section>\n")

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
