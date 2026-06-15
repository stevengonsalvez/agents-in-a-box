#!/usr/bin/env python3
"""Hangar explainer — two tabs: visual Showcase + full Status/Roadmap. Self-contained, Claude palette."""
import base64, pathlib, html

A = pathlib.Path("/tmp/hangar-explainer/assets")
OUT = pathlib.Path("/tmp/hangar-explainer/hangar.html")

def uri(name):
    p = A / name
    mime = "image/gif" if name.endswith(".gif") else "image/png"
    return f"data:{mime};base64," + base64.b64encode(p.read_bytes()).decode()

# ---- TAB 1: SHOWCASE -------------------------------------------------------
SECTIONS = [
    ("board_still.png", "img", "01 · The board",
     "A Linear-style board, in your terminal",
     "Five canonical columns — <b>Backlog · Todo · In&nbsp;Progress · In&nbsp;Review · Done</b>. Every issue is a bordered card carrying its <code>HGR-</code>id, title, priority chip and assignee. The focused card lifts with a clay border; empty columns show a dashed placeholder, not a void."),
    ("drag.gif", "gif", "02 · Drag to move",
     "Grab a card, drop it in another column",
     "A real mouse <b>drag across columns moves the issue</b> — optimistically on screen and <b>durably over the daemon socket</b>, firing the same <code>issue_update{state}</code> RPC the keyboard path uses. <code>Todo&nbsp;(4)→(3)</code>, <code>In&nbsp;Progress&nbsp;(0)→(1)</code> — and it stays after a re-pull."),
    ("rightclick.gif", "gif", "03 · Right-click anything",
     "A context menu on every card",
     "Right-click a card for <b>Open · Move&nbsp;to · Priority · Assign · Copy&nbsp;id · Delete</b>. Each leaf dispatches the real RPC — Move-to → <code>issue_update{state}</code>, Priority → <code>{priority}</code>, Assign → <code>{assignee}</code>. Keyboard-navigable too: mouse and keys share one intent layer."),
    ("clickopen.gif", "gif", "04 · Click to open",
     "Click a card → its task detail",
     "A left-click opens the issue's task detail — status, assignee, project, streamed transcript. Wheel-scroll the columns; pointer-move highlights the card under the cursor. The whole board is mouse-driven — the fact it's a TUI is almost irrelevant."),
]
cards = []
for asset, kind, kicker, title, cap in SECTIONS:
    media = f'<img class="shot" loading="lazy" alt="{html.escape(title)}" src="{uri(asset)}">'
    badge = '<span class="mediakind gif">▶ live mouse capture</span>' if kind == "gif" else '<span class="mediakind">screen</span>'
    cards.append(f"""
    <section class="card">
      <div class="cardhead"><span class="kicker">{kicker}</span>{badge}</div>
      <h3>{title}</h3><figure>{media}</figure><p class="cap">{cap}</p>
    </section>""")

ARCH = '''
<svg viewBox="0 0 770 158" class="arch" role="img" aria-label="Hangar architecture">
  <defs><marker id="ar" markerWidth="9" markerHeight="9" refX="7" refY="3" orient="auto"><path d="M0,0 L7,3 L0,6 Z" fill="#788C5D"/></marker></defs>
  <g font-family="ui-monospace,Menlo,monospace" font-size="12.5" text-anchor="middle">
    <rect x="10"  y="54" width="128" height="52" rx="8" fill="#E3DACC" stroke="#141413" stroke-width="1.5"/>
    <text x="74"  y="76">ainb host</text><text x="74" y="93" font-size="10.5" fill="#5b574e">+ plugin-runtime</text>
    <rect x="186" y="54" width="138" height="52" rx="8" fill="#E3DACC" stroke="#141413" stroke-width="1.5"/>
    <text x="255" y="76">hangar-tui</text><text x="255" y="93" font-size="10.5" fill="#5b574e">plugin · TUI · mouse</text>
    <rect x="384" y="54" width="158" height="52" rx="8" fill="#fff" stroke="#D97757" stroke-width="2"/>
    <text x="463" y="76">hangar-daemon</text><text x="463" y="93" font-size="10.5" fill="#5b574e">claim loop · scheduler</text>
    <rect x="602" y="54" width="120" height="52" rx="8" fill="#E3DACC" stroke="#141413" stroke-width="1.5"/>
    <text x="662" y="76">SQLite</text><text x="662" y="93" font-size="10.5" fill="#5b574e">WAL · 23 migr</text>
    <line x1="138" y1="80" x2="182" y2="80" stroke="#788C5D" stroke-width="2" marker-end="url(#ar)"/>
    <line x1="324" y1="80" x2="380" y2="80" stroke="#788C5D" stroke-width="2" marker-end="url(#ar)"/>
    <line x1="542" y1="80" x2="598" y2="80" stroke="#788C5D" stroke-width="2" marker-end="url(#ar)"/>
    <text x="352" y="44" font-size="10.5" fill="#5b574e">token-gated unix-socket JSON-RPC</text>
    <text x="352" y="128" font-size="10.5" fill="#5b574e">snapshots + event push · capability-gated</text>
  </g>
</svg>'''

# ---- TAB 2: STATUS ---------------------------------------------------------
# feature matrix: (group, [(name, note, state)])  state: done|partial
FEATURES = [
  ("Issue board", [
    ("5-column lifecycle card-board", "Backlog · Todo · In Progress · In Review · Done", "done"),
    ("HGR-N display ids", "per-workspace ordinal at the display layer", "done"),
    ("Priority chips + assignees", "Urgent/High/Medium/None · type:id avatar", "done"),
    ("Filter chips", "All · Members · Agents · Mine", "done"),
    ("Inline create", "c → type → Enter, lands in Todo", "done"),
    ("Full mouse", "drag-move · right-click menu · click-open · scroll · hover", "done"),
  ]),
  ("Run & automate", [
    ("Task Kanban", "queued → running → done → failed, live", "done"),
    ("Daemon claim loop", "claims + executes queued tasks", "done"),
    ("Cron autopilots", "schedule + run-now + enable/disable", "done"),
    ("Task detail + transcript", "status · assignee · project · streamed events", "done"),
  ]),
  ("Agents & skills", [
    ("Skills manager", "per-workspace import, attach to agents", "done"),
    ("Agent templates", "minted from curated templates + bundled skills", "done"),
    ("Skills sync resilience", "one bad SKILL.md aborts the whole import", "partial"),
  ]),
  ("Operate", [
    ("Daemon health pane", "runtime · claim cache · concurrency · throughput sparkline", "done"),
    ("Usage rollup", "token/cost rollup [U]", "done"),
    ("Logs (filterable)", "all/info/warn/error [L]", "done"),
    ("Inbox + unread badge", "aggregated issue/comment/task [I]", "done"),
    ("Settings", "providers · members · workspace [,]", "done"),
    ("Command palette", "Ctrl+P cross-entity fuzzy search", "done"),
    ("Offline empty-state", "guided panel + one-key [s] start", "done"),
  ]),
  ("Platform & hardening", [
    ("Token-gated control socket", "daemon mints a token; plugin authenticates", "done"),
    ("Capability gating", "runtime -32001, not Linker omission", "done"),
    ("Concurrency cap", "no double-claim, no WAL deadlock at the boundary", "done"),
    ("Poisoned-terminal retry taxonomy", "wedged conversation restarts fresh", "done"),
    ("Env allowlist", "blocks LD_PRELOAD into the agent sandbox", "done"),
    ("Soak / backpressure", "500+ events, zero drop/reorder, bounded RSS", "done"),
    ("No-orphan parent-death watcher", "plugins die with the host", "done"),
    ("Render-dirty repaint fix", "dialled-socket data now marks the host dirty", "done"),
  ]),
]
fgroups = []
for group, items in FEATURES:
    rows = "".join(
        f'<tr><td class="ft">{n}</td><td class="fn">{note}</td>'
        f'<td class="fs"><span class="dot {st}"></span>{"shipped" if st=="done" else "follow-up"}</td></tr>'
        for n, note, st in items)
    fgroups.append(f'<div class="fgroup"><h4>{group}</h4><table class="ftable"><tbody>{rows}</tbody></table></div>')

# roadmap: (title, detail, tag)
NEXT = [
  ("Merge PR&nbsp;#250", "e38 parity + 63l board redesign. CI green both legs, merge-state CLEAN — held for a human merge (<code>gh pr merge 250 --merge</code>).", "now"),
  ("Hangar v1.0 release ceremony", "gh integration + e2e pass (<code>174.10</code>), brew formula bump + <code>ainb-hangar-daemon</code> install (<code>174.10.4</code>), release notes + <code>v1.0.0</code> tag (<code>174.10.5</code>).", "next"),
  ("Skills-sync resilience", "one malformed <code>SKILL.md</code> aborts the whole import — make it skip + report (<code>v70</code>, P3). Surfaced live in the demo seed.", "follow-up"),
  ("Per-provider danger warning to TUI", "deliver the warning over the event bus (<code>2qo</code>, P2 — hangar P8 follow-up).", "follow-up"),
  ("Linux plugin-crash / no-orphan tripwire", "portable coverage on the Linux leg (<code>2a8</code>, P2 — follow-up to e38.31).", "follow-up"),
  ("Network egress confinement", "confine the agent sandbox's network (<code>0mf</code>, P2 — follow-up to e38.23).", "follow-up"),
  ("CI hygiene", "<code>beads_adapter</code> concurrent-serialize flake on CI Linux (<code>hp8</code>, P2); enrich demo Autopilots/Skills seed (FK + skills env) for fuller recordings.", "chore"),
]
nextrows = "".join(
  f'<div class="ritem"><span class="rtag {tag}">{tag}</span><div><b>{t}</b><p>{d}</p></div></div>'
  for t, d, tag in NEXT)

TIMELINE = [
  ("174 · hangar-v1", "2026-05-28", "TUI-first managed-agents control plane (Multica replica) — daemon + plugin + SQLite, 77 beads. Build complete; release ceremony pending."),
  ("e38 · parity", "2026-06-09", "Post-v1 gaps from the feature-parity review — 35 + 5 beads, all closed. Token-gated socket, event push, per-(issue,agent) concurrency, soak."),
  ("63l · board redesign", "2026-06-14", "Linear-style 5-column card-board + full mouse across every list screen. All beads closed; found + fixed the render-dirty race."),
  ("v1.0 · release", "next", "Merge #250 → gh integration → brew + daemon install → release notes → v1.0.0 tag."),
]
tlrows = "".join(
  f'<div class="tlitem"><div class="tldate">{d}</div><div class="tlbody"><b>{t}</b><p>{x}</p></div></div>'
  for t, d, x in TIMELINE)

HTML = f"""<!DOCTYPE html>
<html lang="en"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Hangar — control plane: showcase + status</title>
<style>
:root{{--ivory:#FAF9F5;--slate:#141413;--clay:#D97757;--oat:#E3DACC;--olive:#788C5D;--mut:#6b665c;}}
*{{box-sizing:border-box}}
body{{margin:0;background:var(--ivory);color:var(--slate);font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Inter,system-ui,sans-serif;line-height:1.55}}
code,kbd{{font-family:ui-monospace,SFMono-Regular,Menlo,monospace}}
kbd{{background:var(--slate);color:var(--ivory);border-radius:5px;padding:1px 7px;font-size:.82em;font-weight:600}}
code{{background:var(--oat);border-radius:4px;padding:1px 5px;font-size:.88em}}
.wrap{{max-width:1060px;margin:0 auto;padding:0 22px}}
.ok{{color:var(--olive);font-weight:700}}
.tabbar{{position:sticky;top:0;z-index:5;background:var(--ivory);border-bottom:1px solid var(--oat);display:flex;gap:6px;padding:14px 0 0}}
.tabbtn{{appearance:none;border:1px solid var(--oat);border-bottom:none;background:#fff;color:var(--mut);font-weight:700;font-size:.92rem;padding:11px 20px;border-radius:11px 11px 0 0;cursor:pointer}}
.tabbtn[aria-selected=true]{{color:var(--slate);border-color:var(--clay);box-shadow:inset 0 -3px 0 var(--clay)}}
.tab{{display:none}} .tab.on{{display:block}}
header.hero{{padding:36px 0 22px}}
.eyebrow{{color:var(--clay);font-weight:700;letter-spacing:.12em;text-transform:uppercase;font-size:.74rem}}
h1{{font-size:2.5rem;line-height:1.08;margin:.3em 0 .25em;letter-spacing:-.02em}}
.lede{{font-size:1.16rem;color:var(--mut);max-width:66ch;margin:0}} .lede b{{color:var(--slate)}}
.herofig{{margin:28px 0 6px;border:1px solid var(--oat);border-radius:14px;overflow:hidden;background:#000;box-shadow:0 12px 40px rgba(20,20,19,.10)}}
.herofig img{{display:block;width:100%}}
.herocap{{color:var(--mut);font-size:.92rem;margin:10px 2px 0}}
.archbox{{margin:30px 0 8px;padding:16px;border:1px solid var(--oat);border-radius:14px;background:#fff}}
.arch{{width:100%;height:auto;max-height:168px}}
.grid{{display:grid;grid-template-columns:repeat(2,1fr);gap:22px;margin:28px 0}}
.card{{border:1px solid var(--oat);border-radius:14px;background:#fff;padding:18px;display:flex;flex-direction:column}}
.cardhead{{display:flex;justify-content:space-between;align-items:center;margin-bottom:4px}}
.kicker{{color:var(--clay);font-weight:700;font-size:.72rem;letter-spacing:.08em;text-transform:uppercase}}
.mediakind{{font-size:.66rem;font-weight:700;letter-spacing:.06em;text-transform:uppercase;color:var(--mut);background:var(--oat);border-radius:20px;padding:2px 9px}}
.mediakind.gif{{color:#fff;background:var(--clay)}}
.card h3{{margin:.15em 0 .5em;font-size:1.18rem;letter-spacing:-.01em}}
.card figure{{margin:0;border:1px solid var(--oat);border-radius:10px;overflow:hidden;background:#000}}
.shot{{display:block;width:100%}}
.cap{{color:var(--mut);font-size:.92rem;margin:12px 2px 0}} .cap code{{font-size:.85em}}
.callout{{margin:8px 0 30px;border:1px solid var(--oat);border-left:5px solid var(--olive);border-radius:12px;background:#fff;padding:22px 24px}}
.callout h2{{margin:0 0 .5em;font-size:1.35rem}}
.callout ul{{margin:.4em 0 0;padding-left:1.2em}} .callout li{{margin:.3em 0;color:#33312c}}
.stat{{display:inline-flex;gap:8px;flex-wrap:wrap;margin:6px 0 2px}}
.pill{{background:var(--oat);border-radius:20px;padding:4px 12px;font-size:.82rem;font-weight:600}}
.pill.green{{background:#e7eede;color:#3f5230}} .pill.clay{{background:#f6e0d6;color:#9c4a2c}}
.sec{{margin:30px 0}} .sec h2{{font-size:1.45rem;margin:0 0 4px}} .sec .sub{{color:var(--mut);margin:0 0 16px;font-size:.96rem}}
.fmatrix{{display:grid;grid-template-columns:repeat(2,1fr);gap:18px}}
.fgroup{{border:1px solid var(--oat);border-radius:12px;background:#fff;padding:14px 16px}}
.fgroup h4{{margin:.1em 0 .5em;font-size:.96rem;color:var(--clay);text-transform:uppercase;letter-spacing:.06em}}
.ftable{{width:100%;border-collapse:collapse;font-size:.9rem}}
.ftable td{{padding:5px 4px;border-top:1px solid #f0ece3;vertical-align:top}}
.ft{{font-weight:600;width:42%}} .fn{{color:var(--mut);font-size:.85rem}} .fs{{white-space:nowrap;text-align:right;font-size:.8rem;color:var(--mut)}}
.dot{{display:inline-block;width:8px;height:8px;border-radius:50%;margin-right:6px;vertical-align:middle}}
.dot.done{{background:var(--olive)}} .dot.partial{{background:var(--clay)}}
.ritem{{display:flex;gap:14px;align-items:flex-start;border:1px solid var(--oat);border-radius:12px;background:#fff;padding:14px 16px;margin-bottom:12px}}
.ritem p{{margin:.25em 0 0;color:var(--mut);font-size:.92rem}}
.rtag{{flex:0 0 auto;font-size:.66rem;font-weight:800;letter-spacing:.05em;text-transform:uppercase;border-radius:7px;padding:4px 9px;margin-top:2px}}
.rtag.now{{background:var(--clay);color:#fff}} .rtag.next{{background:#f6e0d6;color:#9c4a2c}}
.rtag.follow-up{{background:var(--oat);color:#5b574e}} .rtag.chore{{background:#eee9df;color:#7a7468}}
.tlitem{{display:flex;gap:18px;border-left:2px solid var(--oat);padding:0 0 18px 18px;position:relative}}
.tlitem::before{{content:"";position:absolute;left:-6px;top:4px;width:10px;height:10px;border-radius:50%;background:var(--clay)}}
.tldate{{flex:0 0 92px;color:var(--mut);font-size:.82rem;font-weight:600;padding-top:2px}}
.tlbody p{{margin:.25em 0 0;color:var(--mut);font-size:.92rem}}
footer{{padding:26px 0 70px;color:var(--mut);font-size:.86rem;border-top:1px solid var(--oat);margin-top:18px}}
@media(max-width:760px){{.grid,.fmatrix{{grid-template-columns:1fr}}h1{{font-size:2rem}}}}
</style></head>
<body><div class="wrap">

  <div class="tabbar" role="tablist">
    <button class="tabbtn" role="tab" aria-selected="true" id="t1" onclick="show('showcase')">Showcase</button>
    <button class="tabbtn" role="tab" aria-selected="false" id="t2" onclick="show('status')">Status &amp; roadmap</button>
  </div>

  <!-- ============ TAB 1: SHOWCASE ============ -->
  <div id="tab-showcase" class="tab on">
    <header class="hero">
      <div class="eyebrow">ainb · hangar · board redesign</div>
      <h1>The board went Linear —<br>and it's mouse-driven</h1>
      <p class="lede"><b>Hangar's</b> issue board is now a <b>Linear-style card board</b> you drive with the mouse: <b>drag</b> a card across columns to move the issue, <b>right-click</b> for a context menu, <b>click</b> to open, scroll and hover — every gesture wired to a real daemon RPC. Press <kbd>g</kbd> from the ainb home, then <kbd>1</kbd> for the board.</p>
      <figure class="herofig"><img alt="Hangar grand tour" src="{uri('hero.gif')}"></figure>
      <p class="herocap">The grand tour — <kbd>g</kbd> to open, then tabbing every screen: Issues · Task · Skills&nbsp;<kbd>3</kbd> · Autopilots&nbsp;<kbd>4</kbd> · Kanban&nbsp;<kbd>K</kbd> · Daemon&nbsp;<kbd>D</kbd> · Usage&nbsp;<kbd>U</kbd> · Logs&nbsp;<kbd>L</kbd> · Inbox&nbsp;<kbd>I</kbd> · Settings&nbsp;<kbd>,</kbd>. Every frame is a real capture.</p>
      <div class="archbox">{ARCH}</div>
    </header>
    <div class="grid">{''.join(cards)}</div>
    <div class="callout">
      <h2>Built &amp; verified, not vibed</h2>
      <div class="stat">
        <span class="pill green">epic 63l · all beads closed</span>
        <span class="pill green">CI green · both legs</span>
        <span class="pill green">render + mouse + tmux tests</span>
        <span class="pill green">0 multica refs</span>
      </div>
      <ul>
        <li>Every mouse gesture hits a real seam: drag → <code>MoveCard{{to_status}}</code> → <code>issue_update{{state}}</code>; right-click leaves → <code>issue_update</code> for state / priority / assignee; click → task detail. Mutation-verified — no-op the dispatch and the test goes red.</li>
        <li>Rolled to every list screen — Issues, Task Kanban, Autopilots, Skills. Kanban's drag is intentionally inert (lifecycle is daemon-driven), so a drag never fabricates a fake transition.</li>
        <li>Locked by tests: render goldens per screen, a table-driven <code>plugin/handle_mouse</code> suite, and a real-tmux <b>SGR mouse-drag tripwire</b> asserting the card physically changes columns.</li>
        <li>Found &amp; fixed a real bug doing it: a host race where a plugin reading its own dialled socket never marked the host render-dirty — an async snapshot could sit unpainted. One missing <code>render_dirty.store</code>, mutation-verified fix.</li>
      </ul>
    </div>
  </div>

  <!-- ============ TAB 2: STATUS ============ -->
  <div id="tab-status" class="tab">
    <header class="hero">
      <div class="eyebrow">ainb · hangar · where it stands</div>
      <h1>Status &amp; roadmap</h1>
      <p class="lede"><b>Hangar</b> is a TUI-first managed-agents control plane inside <code>ainb</code> — a daemon + a terminal-UI plugin over a token-gated unix-socket link, backed by SQLite. Below: everything that's shipped, what's verified, and what's next.</p>
      <div class="stat" style="margin-top:18px">
        <span class="pill green">v1 build complete</span>
        <span class="pill green">parity epic e38 · done</span>
        <span class="pill green">board redesign 63l · done</span>
        <span class="pill green">PR #250 · CI green · merge-ready</span>
        <span class="pill clay">v1.0 release · next</span>
      </div>
      <div class="archbox">{ARCH}</div>
    </header>

    <div class="sec">
      <h2>What's shipped</h2>
      <p class="sub">Twelve screens and the platform under them. <span class="ok">●</span> shipped &nbsp; <span style="color:var(--clay)">●</span> follow-up open.</p>
      <div class="fmatrix">{''.join(fgroups)}</div>
    </div>

    <div class="sec">
      <h2>Verified, not asserted</h2>
      <p class="sub">How the above is held green.</p>
      <div class="callout" style="margin:0">
        <ul>
          <li><b>34-tripwire end-to-end suite</b> — real <code>ainb</code> TUI + real daemon + real SQLite in tmux. Full on the Linux leg; a launch-smoke subset on the weaker macOS runner.</li>
          <li><b>Render goldens</b> per list screen + a table-driven <code>plugin/handle_mouse</code> suite + a <b>real-tmux SGR mouse-drag tripwire</b>.</li>
          <li><b>Acceptance gate 79 / 79</b> (proto · store · daemon incl. migration 0023) · <b>335 plugin tests</b> · close-out <b>verify-walk 27 / 0</b> (F01–F44 + R01–R08) on real binaries.</li>
          <li><b>Soak / backpressure</b> — 500+ events, zero drop/reorder, bounded RSS. Concurrency cap holds at the boundary; poisoned-terminal retry taxonomy; env allowlist blocks <code>LD_PRELOAD</code>.</li>
        </ul>
      </div>
    </div>

    <div class="sec">
      <h2>What's next</h2>
      <p class="sub">In priority order. Bead ids in <code>mono</code>.</p>
      {nextrows}
    </div>

    <div class="sec">
      <h2>How it got here</h2>
      <p class="sub">Three epics, build → parity → redesign → release.</p>
      {tlrows}
    </div>
  </div>

  <footer>
    <b>How the media were captured.</b> Driven through the real <code>ainb</code> TUI against a seeded local demo. Mouse journeys use real <b>SGR mouse escape sequences</b> injected over tmux (vhs can't drive a mouse), captured with asciinema + agg; coordinates resolved from <code>capture-pane</code> exactly as the real-tmux mouse-drag tripwire does. Provider execution is mocked via <code>fake-claude.sh</code> — the lifecycle FSM + DB transitions are real, the agent's reasoning is stubbed. Tab keys: Skills&nbsp;<kbd>3</kbd>, Autopilots&nbsp;<kbd>4</kbd>, Usage&nbsp;<kbd>U</kbd>, Inbox&nbsp;<kbd>I</kbd> (post-e38.38). All media are unedited captures.
  </footer>

</div>
<script>
function show(name){{
  for(const t of ['showcase','status']){{
    document.getElementById('tab-'+t).classList.toggle('on', t===name);
  }}
  document.getElementById('t1').setAttribute('aria-selected', name==='showcase');
  document.getElementById('t2').setAttribute('aria-selected', name==='status');
  if(history.replaceState) history.replaceState(null,'','#'+name);
}}
if(location.hash==='#status') show('status');
</script>
</body></html>"""
OUT.write_text(HTML)
print(f"wrote {OUT} ({OUT.stat().st_size//1024} KB)")
