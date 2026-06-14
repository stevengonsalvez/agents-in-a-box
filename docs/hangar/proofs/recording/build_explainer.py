#!/usr/bin/env python3
"""Build the Hangar board-redesign explainer (self-contained, Claude palette)."""
import base64, pathlib, html

A = pathlib.Path("/tmp/hangar-explainer/assets")
OUT = pathlib.Path("/tmp/hangar-explainer/hangar-board-redesign.html")

def uri(name):
    p = A / name
    mime = "image/gif" if name.endswith(".gif") else "image/png"
    return f"data:{mime};base64," + base64.b64encode(p.read_bytes()).decode()

# (asset, kind, kicker, title, caption)
SECTIONS = [
    ("board_still.png", "img", "01 · The board",
     "A Linear-style board, in your terminal",
     "Five canonical columns — <b>Backlog · Todo · In&nbsp;Progress · In&nbsp;Review · Done</b>. Every issue is a bordered card carrying its <code>HGR-</code>id, title, priority chip and assignee. The focused card lifts with a clay border; empty columns show a dashed placeholder, not a void."),
    ("drag.gif", "gif", "02 · Drag to move",
     "Grab a card, drop it in another column",
     "A real mouse <b>drag across columns moves the issue</b> — optimistically on screen and <b>durably over the daemon socket</b>, firing the same <code>issue_update{state}</code> RPC the keyboard path uses. <code>Todo&nbsp;(4)&nbsp;→&nbsp;Todo&nbsp;(3)</code>, <code>In&nbsp;Progress&nbsp;(0)&nbsp;→&nbsp;(1)</code> — and it stays after a re-pull."),
    ("rightclick.gif", "gif", "03 · Right-click anything",
     "A context menu on every card",
     "Right-click a card for <b>Open · Move&nbsp;to · Priority · Assign · Copy&nbsp;id · Delete</b>. Each leaf dispatches the real RPC — Move-to → <code>issue_update{state}</code>, Priority → <code>{priority}</code>, Assign → <code>{assignee}</code>. The menu is keyboard-navigable too: mouse and keys share one intent layer."),
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
      <h3>{title}</h3>
      <figure>{media}</figure>
      <p class="cap">{cap}</p>
    </section>""")

ARCH = '''
<svg viewBox="0 0 770 158" class="arch" role="img" aria-label="Hangar mouse architecture">
  <defs><marker id="ar" markerWidth="9" markerHeight="9" refX="7" refY="3" orient="auto"><path d="M0,0 L7,3 L0,6 Z" fill="#788C5D"/></marker></defs>
  <g font-family="ui-monospace,Menlo,monospace" font-size="12.5" text-anchor="middle">
    <rect x="10"  y="54" width="128" height="52" rx="8" fill="#E3DACC" stroke="#141413" stroke-width="1.5"/>
    <text x="74"  y="76">mouse event</text><text x="74" y="93" font-size="10.5" fill="#5b574e">SGR · viewport-rel</text>
    <rect x="186" y="54" width="138" height="52" rx="8" fill="#E3DACC" stroke="#141413" stroke-width="1.5"/>
    <text x="255" y="76">hangar-tui</text><text x="255" y="93" font-size="10.5" fill="#5b574e">hit-map · FSM · intent</text>
    <rect x="384" y="54" width="158" height="52" rx="8" fill="#fff" stroke="#D97757" stroke-width="2"/>
    <text x="463" y="76">hangar-daemon</text><text x="463" y="93" font-size="10.5" fill="#5b574e">issue_update RPC</text>
    <rect x="602" y="54" width="120" height="52" rx="8" fill="#E3DACC" stroke="#141413" stroke-width="1.5"/>
    <text x="662" y="76">SQLite</text><text x="662" y="93" font-size="10.5" fill="#5b574e">WAL · lifecycle</text>
    <line x1="138" y1="80" x2="182" y2="80" stroke="#788C5D" stroke-width="2" marker-end="url(#ar)"/>
    <line x1="324" y1="80" x2="380" y2="80" stroke="#788C5D" stroke-width="2" marker-end="url(#ar)"/>
    <line x1="542" y1="80" x2="598" y2="80" stroke="#788C5D" stroke-width="2" marker-end="url(#ar)"/>
    <text x="352" y="44" font-size="10.5" fill="#5b574e">drag drop → MoveCard{to_status}</text>
    <text x="352" y="128" font-size="10.5" fill="#5b574e">render re-paints on the daemon's IssueUpdated push</text>
  </g>
</svg>'''

HTML = f"""<!DOCTYPE html>
<html lang="en"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Hangar — the mouse-driven board redesign</title>
<style>
:root{{--ivory:#FAF9F5;--slate:#141413;--clay:#D97757;--oat:#E3DACC;--olive:#788C5D;--mut:#6b665c;}}
*{{box-sizing:border-box}}
body{{margin:0;background:var(--ivory);color:var(--slate);font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Inter,system-ui,sans-serif;line-height:1.55}}
code,kbd{{font-family:ui-monospace,SFMono-Regular,Menlo,monospace}}
kbd{{background:var(--slate);color:var(--ivory);border-radius:5px;padding:1px 7px;font-size:.82em;font-weight:600}}
code{{background:var(--oat);border-radius:4px;padding:1px 5px;font-size:.9em}}
.wrap{{max-width:1060px;margin:0 auto;padding:0 22px}}
.ok{{color:var(--olive);font-weight:700}}
header.hero{{padding:64px 0 28px}}
.eyebrow{{color:var(--clay);font-weight:700;letter-spacing:.12em;text-transform:uppercase;font-size:.74rem}}
h1{{font-size:2.7rem;line-height:1.08;margin:.3em 0 .25em;letter-spacing:-.02em}}
.lede{{font-size:1.18rem;color:var(--mut);max-width:66ch;margin:0}}
.lede b{{color:var(--slate)}}
.herofig{{margin:30px 0 6px;border:1px solid var(--oat);border-radius:14px;overflow:hidden;background:#000;box-shadow:0 12px 40px rgba(20,20,19,.10)}}
.herofig img{{display:block;width:100%}}
.herocap{{color:var(--mut);font-size:.92rem;margin:10px 2px 0}}
.archbox{{margin:34px 0 8px;padding:16px;border:1px solid var(--oat);border-radius:14px;background:#fff}}
.arch{{width:100%;height:auto;max-height:168px}}
.grid{{display:grid;grid-template-columns:repeat(2,1fr);gap:22px;margin:30px 0}}
.card{{border:1px solid var(--oat);border-radius:14px;background:#fff;padding:18px;display:flex;flex-direction:column}}
.cardhead{{display:flex;justify-content:space-between;align-items:center;margin-bottom:4px}}
.kicker{{color:var(--clay);font-weight:700;font-size:.72rem;letter-spacing:.08em;text-transform:uppercase}}
.mediakind{{font-size:.66rem;font-weight:700;letter-spacing:.06em;text-transform:uppercase;color:var(--mut);background:var(--oat);border-radius:20px;padding:2px 9px}}
.mediakind.gif{{color:#fff;background:var(--clay)}}
.card h3{{margin:.15em 0 .5em;font-size:1.18rem;letter-spacing:-.01em}}
.card figure{{margin:0;border:1px solid var(--oat);border-radius:10px;overflow:hidden;background:#000}}
.shot{{display:block;width:100%}}
.cap{{color:var(--mut);font-size:.92rem;margin:12px 2px 0}}
.cap code{{font-size:.85em}}
.callout{{margin:8px 0 30px;border:1px solid var(--oat);border-left:5px solid var(--olive);border-radius:12px;background:#fff;padding:22px 24px}}
.callout h2{{margin:0 0 .5em;font-size:1.35rem}}
.callout ul{{margin:.4em 0 0;padding-left:1.2em}}
.callout li{{margin:.3em 0;color:#33312c}}
.stat{{display:inline-flex;gap:8px;flex-wrap:wrap;margin:6px 0 2px}}
.pill{{background:var(--oat);border-radius:20px;padding:4px 12px;font-size:.82rem;font-weight:600}}
.pill.green{{background:#e7eede;color:#3f5230}}
footer{{padding:30px 0 70px;color:var(--mut);font-size:.86rem;border-top:1px solid var(--oat);margin-top:18px}}
footer code{{font-size:.85em}}
@media(max-width:760px){{.grid{{grid-template-columns:1fr}}h1{{font-size:2rem}}}}
</style></head>
<body>
<div class="wrap">
  <header class="hero">
    <div class="eyebrow">ainb · hangar · board redesign</div>
    <h1>The board went Linear —<br>and it's mouse-driven</h1>
    <p class="lede"><b>Hangar's</b> issue board is now a <b>Linear-style card board</b> you drive with the mouse: <b>drag</b> a card across columns to move the issue, <b>right-click</b> for a context menu, <b>click</b> to open, scroll and hover — every gesture wired to a real daemon RPC. The fact it's a TUI is almost irrelevant. Press <kbd>g</kbd> from the ainb home, then <kbd>1</kbd> for the board.</p>
    <figure class="herofig"><img alt="Hangar grand tour" src="{uri('hero.gif')}"></figure>
    <p class="herocap">The grand tour — <kbd>g</kbd> to open, then tabbing through every screen: Issues · Task · Skills&nbsp;<kbd>3</kbd> · Autopilots&nbsp;<kbd>4</kbd> · Kanban&nbsp;<kbd>K</kbd> · Daemon&nbsp;<kbd>D</kbd> · Usage&nbsp;<kbd>U</kbd> · Logs&nbsp;<kbd>L</kbd> · Inbox&nbsp;<kbd>I</kbd> · Settings&nbsp;<kbd>,</kbd>. Every frame is a real capture of the running TUI.</p>
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
      <li>The redesign landed across epic <code>agents-in-a-box-63l</code> — a shared card-board widget + a render-time hit-map / mouse-FSM / intent layer, every bead an atomic, test-first commit.</li>
      <li><b>Every mouse gesture hits a real seam:</b> drag → <code>MoveCard{{to_status}}</code> → <code>issue_update{{state}}</code> over the daemon socket; right-click leaves → <code>issue_update</code> for state / priority / assignee; click → task detail. Mutation-verified — no-op the dispatch and the test goes red.</li>
      <li><b>Rolled to every list screen</b> — Issues, Task Kanban, Autopilots and Skills all render through the same card-board. Kanban's drag is intentionally inert (task lifecycle is daemon-driven), so a drag never fabricates a fake transition.</li>
      <li><b>Locked by tests:</b> render goldens per screen, a table-driven <code>plugin/handle_mouse</code> suite, and a <b>real-tmux SGR mouse-drag tripwire</b> that drives the actual escape sequences and asserts the card physically changes columns — pruned off the weak macOS runner, run full on Linux.</li>
      <li><b>Found &amp; fixed a real bug doing it:</b> a pre-existing host race — a plugin reading its own dialled daemon socket never marked the host render-dirty, so an async snapshot could sit unpainted (a flaky blank board). One missing <code>render_dirty.store</code> in the unix-socket read loop; fixed with a mutation-verified regression test.</li>
    </ul>
  </div>

  <footer>
    <b>How these were captured.</b> Driven through the real <code>ainb</code> TUI against a seeded local demo workspace. The <b>mouse journeys</b> are real <b>SGR mouse escape sequences</b> (<code>ESC[&lt;0;col;rowM</code> press, <code>&lt;32</code> drag, <code>&lt;0..m</code> release) injected into the live TUI over tmux — vhs can't drive a mouse — captured with asciinema + agg. Provider execution is mocked via a tiny <code>fake-claude.sh</code>; the lifecycle FSM and DB transitions are real, the agent's reasoning is stubbed. Tab keys reflect the post-<code>e38.38</code> renumber: Skills&nbsp;<kbd>3</kbd>, Autopilots&nbsp;<kbd>4</kbd>, Usage&nbsp;<kbd>U</kbd>, Inbox&nbsp;<kbd>I</kbd>. &nbsp;·&nbsp; Architecture diagram hand-drawn; all media are unedited captures.
  </footer>
</div>
</body></html>"""

OUT.write_text(HTML)
print(f"wrote {OUT} ({OUT.stat().st_size//1024} KB)")
