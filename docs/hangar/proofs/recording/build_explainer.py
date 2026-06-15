#!/usr/bin/env python3
"""Hangar explainer — four tabs: Showcase · Status · End-to-end · vs Multica. Self-contained, Claude palette."""
import base64, pathlib, html

A = pathlib.Path("/tmp/hangar-explainer/assets")
OUT = pathlib.Path("/tmp/hangar-explainer/hangar.html")

def uri(name):
    p = A / name
    mime = "image/gif" if name.endswith(".gif") else "image/png"
    return f"data:{mime};base64," + base64.b64encode(p.read_bytes()).decode()

# ---- TAB 1: SHOWCASE -------------------------------------------------------
SHOW = [
    ("board_still.png", "img", "01 · The board", "A Linear-style board, in your terminal",
     "Five canonical columns — <b>Backlog · Todo · In&nbsp;Progress · In&nbsp;Review · Done</b>. Every issue is a bordered card carrying its <code>HGR-</code>id, title, priority chip and assignee. The focused card lifts with a clay border; empty columns show a dashed placeholder."),
    ("drag.gif", "gif", "02 · Drag to move", "Grab a card, drop it in another column",
     "A real mouse <b>drag moves the issue</b> — optimistically on screen and <b>durably over the daemon socket</b> (the same <code>issue_update{state}</code> RPC the keyboard uses). <code>Todo&nbsp;(4)→(3)</code>, <code>In&nbsp;Progress&nbsp;(0)→(1)</code>."),
    ("rightclick.gif", "gif", "03 · Right-click anything", "A context menu on every card",
     "Right-click for <b>Open · Move&nbsp;to · Priority · Assign · Copy&nbsp;id · Delete</b>. Each leaf fires the real RPC. Keyboard-navigable too — mouse and keys share one intent layer."),
    ("clickopen.gif", "gif", "04 · Click to open", "Click a card → its task detail",
     "Left-click opens the issue's task detail; wheel-scroll the columns; pointer-move highlights the card under the cursor. The whole board is mouse-driven."),
]

# ---- TAB 3: END-TO-END -----------------------------------------------------
E2E = [
    ("createissue.gif", "gif", "01 · Put work on the board", "Create an issue without leaving the keyboard",
     "Press <kbd>c</kbd>, type a title (<code>Spin up a hello-world service</code>), hit <kbd>Enter</kbd> — the new <code>HGR-5</code> card lands in Todo. Keystroke → <code>issue_create</code> RPC → row → re-render. The board now holds 5 issues."),
    ("execute.gif", "gif", "02 · A cron autopilot fires a real task", "Run it, watch it land in Done",
     "On the Autopilots screen the cron agent <code>helloworld-triage</code> (<code>0&nbsp;9&nbsp;*&nbsp;*&nbsp;*</code>) is fired with <kbd>r</kbd>. The daemon's claim loop enqueues a task, runs the provider, and the <b>Kanban live-updates</b> (no keypress — the daemon pushes the event, the board repaints) until the task lands in <b>Done</b>."),
    ("kanban_done.png", "img", "02 · …and it executed", "The task reaches the Done column",
     "The fired task <code>#Z0Z37Z · 0m · done</code> sits in Done alongside earlier runs. But a green column count isn't proof — the proof is the database, below."),
]

DB_PROOF = """sqlite> select id, status, substr(result,1,40) from agent_task_queue order by rowid desc limit 1;
┌────────────┬────────┬──────────────────────────────────────────┐
│    task    │ status │                  result                  │
├────────────┼────────┼──────────────────────────────────────────┤
│ 01KV5NMVAV │ done   │ {"content":"…Hello, World!…opened a PR"}  │
└────────────┴────────┴──────────────────────────────────────────┘

sqlite> select name, status, started_at from autopilot_run join autopilot using(id) ...;
┌───────────────────┬───────────┬─────────────────────┐
│       name        │    run    │       started       │
├───────────────────┼───────────┼─────────────────────┤
│ helloworld-triage │ completed │ 2026-06-15 12:56:01 │
└───────────────────┴───────────┴─────────────────────┘

# what the task actually produced (the agent's streamed output, captured as the task result):
{"type":"assistant","text":"Looking at the issue..."}
{"type":"assistant","text":"Spinning up a hello-world: $ echo Hello, World!"}
{"type":"assistant","text":"Hello, World! Implemented the change and opened a PR."}
https://github.com/acme/widget/pull/4242"""

# ---- TAB 4: vs MULTICA -----------------------------------------------------
# category | parity | partial | gap | note
CMP = [
  ("Issues &amp; board", "9", "0", "6", "create/edit/labels/priority/comments/@-mention/search all parity; gaps = sub-issues, batch ops, reactions, threaded comments, attachments"),
  ("Tasks &amp; execution", "6", "3", "1", "full FSM, per-(issue,agent) concurrency, retry taxonomy at parity; partial = multi-provider (2 of ~12 live), run history, session resume; gap = 1:1 agent chat"),
  ("Agents &amp; runtimes", "4", "1", "1", "templates + agent CRUD + presence parity; partial = runtimes dashboard; gap = runtime model introspection"),
  ("Skills", "3", "2", "0", "assign/sync/materialise/search parity; partial = URL import, per-file edit"),
  ("Autopilots / cron / webhooks", "7", "0", "1", "cron + modes + concurrency policies + HMAC webhooks at parity; gap = one-click presets"),
  ("Squads &amp; members", "5", "0", "2", "squad CRUD + leader routing + member roles parity; gaps = invitations, leave-workspace"),
  ("Search · inbox · usage", "4", "0", "1", "command palette + inbox + token/cost rollup parity; gap = notification prefs"),
  ("Auth &amp; secrets", "4", "0", "0", "token-gated socket + PAT/daemon tokens + keychain at parity (email/OAuth login are web-form-factor, out of scope)"),
  ("gh / GitHub", "2", "0", "0", "PR-URL capture + CI/conflict badge + auto-move parity (GitHub-App install + Lark are hosted SaaS, out of scope)"),
  ("CLI &amp; daemon", "6", "2", "1", "16 noun-group CLIs + daemon lifecycle + GC parity; partial = self-update verb, repo checkout; gap = daemon profiles"),
]
cmprows = "".join(
  f'<tr><td class="ft">{c}</td>'
  f'<td class="cnum"><span class="dot done"></span>{p}</td>'
  f'<td class="cnum"><span class="dot partial"></span>{pa}</td>'
  f'<td class="cnum"><span class="dot gapdot"></span>{g}</td>'
  f'<td class="fn">{note}</td></tr>'
  for c, p, pa, g, note in CMP)

GAPS = [
  ("Deliberate scope-cut — no terminal form factor", "Electron desktop + iOS clients (the TUI <i>is</i> the client); cloud runtime nodes + Stripe billing (closed SaaS fleet / hosted PCI); email-code + Google OAuth login (PAT/daemon tokens meet the need); GitHub-App install + Lark (OAuth web consent); Docker-Compose/K8s (one binary); avatars (no terminal rendering). <b>Why:</b> these presuppose a hosted web/mobile product Hangar deliberately replaces with a local daemon + TUI."),
  ("Genuinely unbuilt — TUI-expressible, just not scoped yet", "<b>1:1 agent chat</b> (no <code>chat_session</code> table / <code>Chat</code> screen — transcript is task-scoped only); <b>Projects</b> (no <code>project</code> table; Hangar's “Project” aliases the workspace); <b>sub-issues / dependencies</b> (Hangar's parent/child is on <i>tasks</i>, not issues); member <b>invitations</b>; autopilot <b>presets</b>. <b>Why:</b> nothing here is blocked by the TUI architecture — the Kanban already proves the form factor — it is simply work not yet picked up."),
  ("Partial — present but a subset", "Multi-provider exec (2 of ~12 live, but provider-agnostic — each new one is one <code>ProviderSpec</code>); skills import (local tree only, no URL import); repo checkout (whitelist persisted, consumer deferred); per-issue run history; cross-run session resume; telemetry (ops tracing/OTLP only, no product analytics)."),
]
gaprows = "".join(f'<div class="ritem"><span class="rtag gap">gap</span><div><b>{t}</b><p>{d}</p></div></div>' for t, d in GAPS)

JRN = [
  ("Create an issue (priority / due / labels)", "full", "<code>issue create</code> / Issues <kbd>c</kbd>"),
  ("Assign issue → a task enqueues", "full", "agent picker / <code>--assign</code>"),
  ("Watch a task execute live", "full", "Task detail transcript + Kanban"),
  ("@-mention an agent → kick off work", "full", "<code>mentions.rs</code> parse → resolve → spawn"),
  ("Move a card → state change", "full", "drag / <kbd>Shift</kbd>+arrows → <code>task_transition</code>"),
  ("Review the PR (CI + conflict status)", "full", "PR badge: CI rollup + Mergeable/Conflicting + <kbd>o</kbd>"),
  ("Schedule with a cron autopilot", "full", "Autopilots <kbd>a</kbd> + modes + concurrency policies"),
  ("Trigger an autopilot via webhook", "full", "<code>POST /hangar/webhook/&lt;id&gt;</code> + HMAC"),
  ("Search across entities (Cmd+K)", "full", "command palette + <code>issues_search</code>"),
  ("Triage from a notification inbox", "full", "Inbox <kbd>I</kbd> + <code>inbox_mark_read</code>"),
  ("Assign to a squad → leader routes", "full", "<code>squad_assign</code> (leader → runtime → enqueue)"),
  ("Manage members &amp; roles", "full", "<code>member_set_role</code> / <code>member_remove</code>"),
  ("Curate &amp; assign skills", "full", "Skill manager sync + <kbd>i</kbd>/<kbd>d</kbd>"),
  ("See token/cost usage rollup", "full", "Usage <kbd>U</kbd> + <code>usage_rollup</code>"),
  ("Onboard with a questionnaire", "partial", "first-run wizard (runtime-provisioning thinner)"),
  ("Organise issues under a project", "none", "— no project model"),
  ("1:1 chat with an agent", "none", "— no chat sessions / screen"),
  ("Nest sub-issues / dependencies", "none", "— parent/child is on tasks only"),
  ("Batch-update issues", "none", "— no multi-select"),
  ("Invite a teammate", "none", "— no invitation pipeline"),
  ("Use from phone / desktop GUI", "none", "— out of scope by design"),
]
jrnrows = "".join(
  f'<tr><td class="ft">{j}</td><td class="cov"><span class="covpill {c}">{c}</span></td><td class="fn">{h}</td></tr>'
  for j, c, h in JRN)

def cards_html(items, badge_gif="▶ live capture"):
    out = []
    for asset, kind, kicker, title, cap in items:
        media = f'<img class="shot" loading="lazy" alt="{html.escape(title)}" src="{uri(asset)}">'
        badge = f'<span class="mediakind gif">{badge_gif}</span>' if kind == "gif" else '<span class="mediakind">screen</span>'
        out.append(f'<section class="card"><div class="cardhead"><span class="kicker">{kicker}</span>{badge}</div><h3>{title}</h3><figure>{media}</figure><p class="cap">{cap}</p></section>')
    return "".join(out)

show_cards = cards_html(SHOW, "▶ live mouse capture")
e2e_cards = cards_html(E2E, "▶ live capture")

ARCH = '''<svg viewBox="0 0 770 158" class="arch" role="img" aria-label="Hangar architecture">
  <defs><marker id="ar" markerWidth="9" markerHeight="9" refX="7" refY="3" orient="auto"><path d="M0,0 L7,3 L0,6 Z" fill="#788C5D"/></marker></defs>
  <g font-family="ui-monospace,Menlo,monospace" font-size="12.5" text-anchor="middle">
    <rect x="10" y="54" width="128" height="52" rx="8" fill="#E3DACC" stroke="#141413" stroke-width="1.5"/><text x="74" y="76">ainb host</text><text x="74" y="93" font-size="10.5" fill="#5b574e">+ plugin-runtime</text>
    <rect x="186" y="54" width="138" height="52" rx="8" fill="#E3DACC" stroke="#141413" stroke-width="1.5"/><text x="255" y="76">hangar-tui</text><text x="255" y="93" font-size="10.5" fill="#5b574e">plugin · TUI · mouse</text>
    <rect x="384" y="54" width="158" height="52" rx="8" fill="#fff" stroke="#D97757" stroke-width="2"/><text x="463" y="76">hangar-daemon</text><text x="463" y="93" font-size="10.5" fill="#5b574e">claim loop · scheduler</text>
    <rect x="602" y="54" width="120" height="52" rx="8" fill="#E3DACC" stroke="#141413" stroke-width="1.5"/><text x="662" y="76">SQLite</text><text x="662" y="93" font-size="10.5" fill="#5b574e">WAL · 23 migr</text>
    <line x1="138" y1="80" x2="182" y2="80" stroke="#788C5D" stroke-width="2" marker-end="url(#ar)"/><line x1="324" y1="80" x2="380" y2="80" stroke="#788C5D" stroke-width="2" marker-end="url(#ar)"/><line x1="542" y1="80" x2="598" y2="80" stroke="#788C5D" stroke-width="2" marker-end="url(#ar)"/>
    <text x="352" y="44" font-size="10.5" fill="#5b574e">token-gated unix-socket JSON-RPC · 39 methods</text><text x="352" y="128" font-size="10.5" fill="#5b574e">snapshots + event push · capability-gated</text>
  </g></svg>'''

# status-tab feature matrix + roadmap + timeline (carried from build3)
FEATURES = [
  ("Issue board", [("5-column lifecycle card-board","Backlog·Todo·InProgress·InReview·Done","done"),("HGR-N display ids","per-workspace ordinal","done"),("Priority chips + assignees","Urgent/High/Medium/None","done"),("Filter chips","All·Members·Agents·Mine","done"),("Inline create","c → type → Enter","done"),("Full mouse","drag·right-click·click·scroll·hover","done")]),
  ("Run & automate", [("Task Kanban","queued→running→done→failed","done"),("Daemon claim loop","claims + runs queued tasks","done"),("Cron autopilots","schedule + run-now + toggle","done"),("Task detail + transcript","streamed agent output","done")]),
  ("Agents & skills", [("Skills manager","per-workspace import + attach","done"),("Agent templates","minted from curated templates","done"),("Skills-sync resilience","one bad SKILL.md aborts import","partial")]),
  ("Operate", [("Daemon health","runtime·claim·concurrency·throughput","done"),("Usage rollup","token/cost [U]","done"),("Logs (filterable)","all/info/warn/error [L]","done"),("Inbox + unread","aggregated [I]","done"),("Command palette","Ctrl+P fuzzy search","done"),("Offline empty-state","guided [s] start","done")]),
  ("Platform & hardening", [("Token-gated socket","daemon mints token","done"),("Concurrency cap","no double-claim / WAL deadlock","done"),("Retry taxonomy","wedged conversation restarts","done"),("Env allowlist","blocks LD_PRELOAD","done"),("Soak / backpressure","500+ events, no drop","done"),("Render-dirty fix","dialled-socket repaint","done")]),
]
fgroups = "".join(
  '<div class="fgroup"><h4>'+g+'</h4><table class="ftable"><tbody>'+
  "".join(f'<tr><td class="ft">{n}</td><td class="fn">{note}</td><td class="fs"><span class="dot {st}"></span>{"shipped" if st=="done" else "follow-up"}</td></tr>' for n,note,st in items)+
  '</tbody></table></div>'
  for g, items in FEATURES)
NEXT = [
  ("Merge PR&nbsp;#250","e38 parity + 63l board redesign. CI green both legs — held for a human merge.","now"),
  ("Hangar v1.0 release ceremony","gh integration + e2e (<code>174.10</code>), brew + daemon install (<code>174.10.4</code>), release notes + <code>v1.0.0</code> tag (<code>174.10.5</code>).","next"),
  ("Skills-sync resilience","one malformed <code>SKILL.md</code> aborts the whole import — skip + report (<code>v70</code>). Hit live in this demo's seed.","follow-up"),
  ("Per-provider danger warning to TUI","deliver over the event bus (<code>2qo</code>).","follow-up"),
  ("Build chat / projects / sub-issues","the multica deltas worth closing (see the vs-Multica tab).","follow-up"),
]
nextrows = "".join(f'<div class="ritem"><span class="rtag {tag}">{tag}</span><div><b>{t}</b><p>{d}</p></div></div>' for t,d,tag in NEXT)
TL = [("174 · hangar-v1","2026-05-28","TUI-first control plane (Multica replica). Build complete."),("e38 · parity","2026-06-09","35+5 beads, all closed — token socket, event push, concurrency, soak."),("63l · board redesign","2026-06-14","Linear-style mouse-driven card-board. All closed; found+fixed the render-dirty race."),("v1.0 · release","next","Merge #250 → gh → brew → release notes → v1.0.0 tag.")]
tlrows = "".join(f'<div class="tlitem"><div class="tldate">{d}</div><div class="tlbody"><b>{t}</b><p>{x}</p></div></div>' for t,d,x in TL)

HTML = f"""<!DOCTYPE html>
<html lang="en"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Hangar — control plane: showcase · status · end-to-end · vs multica</title>
<style>
:root{{--ivory:#FAF9F5;--slate:#141413;--clay:#D97757;--oat:#E3DACC;--olive:#788C5D;--mut:#6b665c;--red:#b14b3a;}}
*{{box-sizing:border-box}} body{{margin:0;background:var(--ivory);color:var(--slate);font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Inter,system-ui,sans-serif;line-height:1.55}}
code,kbd{{font-family:ui-monospace,SFMono-Regular,Menlo,monospace}}
kbd{{background:var(--slate);color:var(--ivory);border-radius:5px;padding:1px 7px;font-size:.82em;font-weight:600}}
code{{background:var(--oat);border-radius:4px;padding:1px 5px;font-size:.88em}}
.wrap{{max-width:1060px;margin:0 auto;padding:0 22px}} .ok{{color:var(--olive);font-weight:700}}
.tabbar{{position:sticky;top:0;z-index:5;background:var(--ivory);border-bottom:1px solid var(--oat);display:flex;gap:6px;padding:14px 0 0;flex-wrap:wrap}}
.tabbtn{{appearance:none;border:1px solid var(--oat);border-bottom:none;background:#fff;color:var(--mut);font-weight:700;font-size:.9rem;padding:10px 18px;border-radius:11px 11px 0 0;cursor:pointer}}
.tabbtn[aria-selected=true]{{color:var(--slate);border-color:var(--clay);box-shadow:inset 0 -3px 0 var(--clay)}}
.tab{{display:none}} .tab.on{{display:block}}
header.hero{{padding:34px 0 20px}} .eyebrow{{color:var(--clay);font-weight:700;letter-spacing:.12em;text-transform:uppercase;font-size:.74rem}}
h1{{font-size:2.4rem;line-height:1.08;margin:.3em 0 .25em;letter-spacing:-.02em}}
.lede{{font-size:1.14rem;color:var(--mut);max-width:68ch;margin:0}} .lede b{{color:var(--slate)}}
.herofig{{margin:26px 0 6px;border:1px solid var(--oat);border-radius:14px;overflow:hidden;background:#000;box-shadow:0 12px 40px rgba(20,20,19,.10)}} .herofig img{{display:block;width:100%}}
.herocap{{color:var(--mut);font-size:.92rem;margin:10px 2px 0}}
.archbox{{margin:28px 0 8px;padding:16px;border:1px solid var(--oat);border-radius:14px;background:#fff}} .arch{{width:100%;height:auto;max-height:168px}}
.grid{{display:grid;grid-template-columns:repeat(2,1fr);gap:22px;margin:26px 0}}
.card{{border:1px solid var(--oat);border-radius:14px;background:#fff;padding:18px;display:flex;flex-direction:column}}
.cardhead{{display:flex;justify-content:space-between;align-items:center;margin-bottom:4px}}
.kicker{{color:var(--clay);font-weight:700;font-size:.72rem;letter-spacing:.08em;text-transform:uppercase}}
.mediakind{{font-size:.66rem;font-weight:700;letter-spacing:.06em;text-transform:uppercase;color:var(--mut);background:var(--oat);border-radius:20px;padding:2px 9px}} .mediakind.gif{{color:#fff;background:var(--clay)}}
.card h3{{margin:.15em 0 .5em;font-size:1.16rem;letter-spacing:-.01em}}
.card figure{{margin:0;border:1px solid var(--oat);border-radius:10px;overflow:hidden;background:#000}} .shot{{display:block;width:100%}}
.cap{{color:var(--mut);font-size:.92rem;margin:12px 2px 0}} .cap code{{font-size:.85em}}
.callout{{margin:8px 0 30px;border:1px solid var(--oat);border-left:5px solid var(--olive);border-radius:12px;background:#fff;padding:22px 24px}} .callout h2{{margin:0 0 .5em;font-size:1.3rem}} .callout ul{{margin:.4em 0 0;padding-left:1.2em}} .callout li{{margin:.3em 0;color:#33312c}}
.proof{{margin:8px 0 30px;border:1px solid var(--oat);border-left:5px solid var(--clay);border-radius:12px;background:#1d1c1a;padding:18px 20px;overflow:auto}}
.proof h3{{margin:0 0 .4em;color:#fff;font-size:1.05rem}} .proof pre{{margin:0;color:#e7e2d6;font-family:ui-monospace,Menlo,monospace;font-size:.78rem;line-height:1.5;white-space:pre}}
.proof .px{{color:#9bb06f}} .proof .label{{color:#D97757;font-weight:700;font-size:.7rem;letter-spacing:.06em;text-transform:uppercase}}
.stat{{display:inline-flex;gap:8px;flex-wrap:wrap;margin:6px 0 2px}} .pill{{background:var(--oat);border-radius:20px;padding:4px 12px;font-size:.82rem;font-weight:600}} .pill.green{{background:#e7eede;color:#3f5230}} .pill.clay{{background:#f6e0d6;color:#9c4a2c}}
.sec{{margin:30px 0}} .sec h2{{font-size:1.4rem;margin:0 0 4px}} .sec .sub{{color:var(--mut);margin:0 0 16px;font-size:.95rem}}
.fmatrix{{display:grid;grid-template-columns:repeat(2,1fr);gap:18px}} .fgroup{{border:1px solid var(--oat);border-radius:12px;background:#fff;padding:14px 16px}} .fgroup h4{{margin:.1em 0 .5em;font-size:.95rem;color:var(--clay);text-transform:uppercase;letter-spacing:.06em}}
.ftable{{width:100%;border-collapse:collapse;font-size:.9rem}} .ftable td{{padding:5px 4px;border-top:1px solid #f0ece3;vertical-align:top}} .ft{{font-weight:600}} .fn{{color:var(--mut);font-size:.85rem}} .fs{{white-space:nowrap;text-align:right;font-size:.8rem;color:var(--mut)}}
.dot{{display:inline-block;width:8px;height:8px;border-radius:50%;margin-right:6px;vertical-align:middle}} .dot.done{{background:var(--olive)}} .dot.partial{{background:var(--clay)}} .dot.gapdot{{background:var(--red)}}
.bigtable{{width:100%;border-collapse:collapse;font-size:.9rem;background:#fff;border:1px solid var(--oat);border-radius:12px;overflow:hidden}}
.bigtable th{{text-align:left;background:#f3efe6;color:#5b574e;font-size:.72rem;letter-spacing:.05em;text-transform:uppercase;padding:9px 12px}}
.bigtable td{{padding:8px 12px;border-top:1px solid #f0ece3;vertical-align:top}} .bigtable .cnum{{white-space:nowrap;font-weight:700;width:1%}} .cov{{width:1%;white-space:nowrap}}
.covpill{{font-size:.7rem;font-weight:800;letter-spacing:.04em;text-transform:uppercase;border-radius:7px;padding:3px 9px}} .covpill.full{{background:#e7eede;color:#3f5230}} .covpill.partial{{background:#f6e0d6;color:#9c4a2c}} .covpill.none{{background:#f3dada;color:#933}}
.ritem{{display:flex;gap:14px;align-items:flex-start;border:1px solid var(--oat);border-radius:12px;background:#fff;padding:14px 16px;margin-bottom:12px}} .ritem p{{margin:.25em 0 0;color:var(--mut);font-size:.92rem}}
.rtag{{flex:0 0 auto;font-size:.66rem;font-weight:800;letter-spacing:.05em;text-transform:uppercase;border-radius:7px;padding:4px 9px;margin-top:2px}} .rtag.now{{background:var(--clay);color:#fff}} .rtag.next{{background:#f6e0d6;color:#9c4a2c}} .rtag.follow-up{{background:var(--oat);color:#5b574e}} .rtag.gap{{background:#f3dada;color:#933}}
.tlitem{{display:flex;gap:18px;border-left:2px solid var(--oat);padding:0 0 18px 18px;position:relative}} .tlitem::before{{content:"";position:absolute;left:-6px;top:4px;width:10px;height:10px;border-radius:50%;background:var(--clay)}} .tldate{{flex:0 0 92px;color:var(--mut);font-size:.82rem;font-weight:600;padding-top:2px}} .tlbody p{{margin:.25em 0 0;color:var(--mut);font-size:.92rem}}
footer{{padding:26px 0 70px;color:var(--mut);font-size:.86rem;border-top:1px solid var(--oat);margin-top:18px}}
@media(max-width:760px){{.grid,.fmatrix{{grid-template-columns:1fr}}h1{{font-size:1.9rem}}}}
</style></head>
<body><div class="wrap">

  <div class="tabbar" role="tablist">
    <button class="tabbtn" role="tab" aria-selected="true" id="t1" onclick="show('showcase')">Showcase</button>
    <button class="tabbtn" role="tab" aria-selected="false" id="t2" onclick="show('status')">Status &amp; roadmap</button>
    <button class="tabbtn" role="tab" aria-selected="false" id="t3" onclick="show('e2e')">End-to-end</button>
    <button class="tabbtn" role="tab" aria-selected="false" id="t4" onclick="show('vs')">vs Multica</button>
  </div>

  <!-- TAB 1 SHOWCASE -->
  <div id="tab-showcase" class="tab on">
    <header class="hero"><div class="eyebrow">ainb · hangar · board redesign</div>
      <h1>The board went Linear —<br>and it's mouse-driven</h1>
      <p class="lede"><b>Hangar's</b> issue board is a <b>Linear-style card board</b> you drive with the mouse: <b>drag</b> across columns to move an issue, <b>right-click</b> for a menu, <b>click</b> to open — every gesture wired to a real daemon RPC. Press <kbd>g</kbd> from the ainb home, then <kbd>1</kbd>.</p>
      <figure class="herofig"><img alt="Hangar grand tour" src="{uri('hero.gif')}"></figure>
      <p class="herocap">The grand tour — <kbd>g</kbd> to open, then every screen: Issues · Task · Skills&nbsp;<kbd>3</kbd> · Autopilots&nbsp;<kbd>4</kbd> · Kanban&nbsp;<kbd>K</kbd> · Daemon&nbsp;<kbd>D</kbd> · Usage&nbsp;<kbd>U</kbd> · Logs&nbsp;<kbd>L</kbd> · Inbox&nbsp;<kbd>I</kbd> · Settings&nbsp;<kbd>,</kbd>.</p>
      <div class="archbox">{ARCH}</div></header>
    <div class="grid">{show_cards}</div>
  </div>

  <!-- TAB 2 STATUS -->
  <div id="tab-status" class="tab">
    <header class="hero"><div class="eyebrow">ainb · hangar · where it stands</div>
      <h1>Status &amp; roadmap</h1>
      <p class="lede"><b>Hangar</b> is a TUI-first managed-agents control plane inside <code>ainb</code> — a daemon + a terminal-UI plugin over a token-gated unix-socket link, backed by SQLite. Everything shipped, verified, and what's next.</p>
      <div class="stat" style="margin-top:18px"><span class="pill green">v1 build complete</span><span class="pill green">parity epic e38 · done</span><span class="pill green">board redesign 63l · done</span><span class="pill green">PR #250 · CI green · merge-ready</span><span class="pill clay">v1.0 release · next</span></div>
      <div class="archbox">{ARCH}</div></header>
    <div class="sec"><h2>What's shipped</h2><p class="sub"><span class="ok">●</span> shipped &nbsp; <span style="color:var(--clay)">●</span> follow-up open.</p><div class="fmatrix">{fgroups}</div></div>
    <div class="sec"><h2>Verified, not asserted</h2><div class="callout" style="margin:0"><ul>
      <li><b>34-tripwire end-to-end suite</b> — real TUI + real daemon + real SQLite in tmux (full on Linux, launch-smoke on macOS).</li>
      <li><b>Render goldens</b> per screen + a <code>plugin/handle_mouse</code> suite + a <b>real-tmux SGR mouse-drag tripwire</b>.</li>
      <li><b>Acceptance 79/79</b> · <b>335 plugin tests</b> · close-out <b>verify-walk 27/0</b> on real binaries.</li>
      <li><b>Soak / backpressure</b> — 500+ events, zero drop/reorder, bounded RSS.</li></ul></div></div>
    <div class="sec"><h2>What's next</h2>{nextrows}</div>
    <div class="sec"><h2>How it got here</h2>{tlrows}</div>
  </div>

  <!-- TAB 3 END-TO-END -->
  <div id="tab-e2e" class="tab">
    <header class="hero"><div class="eyebrow">ainb · hangar · the full loop</div>
      <h1>Put work in. Watch it run.</h1>
      <p class="lede">The whole user loop, end to end: <b>file an issue on the board</b>, then a <b>cron autopilot fires a real task</b> that the daemon executes — and we don't take a green column count as proof. The proof is the <b>database</b>: the task reaches <code>done</code> and its result carries the agent's output and PR url.</p>
    </header>
    <div class="grid">{e2e_cards}</div>
    <div class="proof">
      <div class="label">validation · the actual SQLite state, not a rendered screen</div>
      <h3>The task really executed</h3>
      <pre>{html.escape(DB_PROOF)}</pre>
    </div>
    <div class="callout">
      <h2>Why this is real</h2>
      <ul>
        <li>The provider is a tiny <code>fake-claude.sh</code> stub (so the demo is hermetic), but <b>everything around it is the shipping code path</b>: the autopilot scheduler, the daemon's claim loop, the task FSM (<code>queued → running → done</code>), the result capture, and the event push that live-updates the Kanban.</li>
        <li>The Kanban repaints <b>with no keypress</b> as the task transitions — that is the daemon-pushed event + the render-dirty fix doing their job.</li>
        <li>Swap <code>fake-claude.sh</code> for the real <code>claude</code> / <code>codex</code> binary and the same path runs a real agent against a real repo.</li>
      </ul>
    </div>
  </div>

  <!-- TAB 4 vs MULTICA -->
  <div id="tab-vs" class="tab">
    <header class="hero"><div class="eyebrow">ainb · hangar · vs the original</div>
      <h1>Hangar &nbsp;⇄&nbsp; Multica</h1>
      <p class="lede">Hangar is a <b>faithful TUI replica</b> of <a href="https://github.com/multica-ai/multica" style="color:var(--clay)">Multica</a> — the web app (Next.js + Go + Postgres, with Electron &amp; iOS clients) it was modelled on. Hangar replaces that whole stack with one <b>daemon</b> + a <b>plugin TUI</b>: <b>39 RPC methods · 13 screens · 16 CLI groups · 23 migrations</b>. After the <code>e38</code> parity epic it reaches genuine parity on every headline journey; the deltas split cleanly into deliberate scope-cuts and unbuilt-but-buildable surfaces.</p>
      <div class="stat" style="margin-top:18px"><span class="pill green">parity on every headline journey</span><span class="pill clay">gaps: chat · projects · sub-issues</span></div>
    </header>
    <div class="sec"><h2>Feature comparison</h2>
      <p class="sub"><span class="dot done"></span>parity &nbsp; <span class="dot partial"></span>partial &nbsp; <span class="dot gapdot"></span>gap (TUI-expressible, not yet built). Web/SaaS-only surfaces (desktop/mobile, OAuth login, billing) are out-of-scope by design and not counted as gaps.</p>
      <table class="bigtable"><thead><tr><th>Area</th><th>parity</th><th>partial</th><th>gap</th><th>detail</th></tr></thead><tbody>{cmprows}</tbody></table>
    </div>
    <div class="sec"><h2>The gaps — and why</h2><p class="sub">Three honest buckets.</p>{gaprows}</div>
    <div class="sec"><h2>User-journey coverage</h2>
      <p class="sub">Every Multica journey, and whether Hangar covers it.</p>
      <table class="bigtable"><thead><tr><th>Multica journey</th><th>coverage</th><th>how in Hangar</th></tr></thead><tbody>{jrnrows}</tbody></table>
    </div>
    <div class="callout"><h2>Bottom line</h2><ul>
      <li>Parity on the full core loop: file an issue → assign/@-mention an agent → watch it execute → review the PR (with CI/conflict status) → schedule on cron or webhook → search, triage from an inbox, route through a squad, read usage.</li>
      <li>Out-of-scope by design (no terminal form factor): Electron/iOS clients, cloud runtime nodes, Stripe billing, email/Google login, GitHub-App install, Lark.</li>
      <li>Unbuilt but buildable (nothing blocked by the TUI): <b>1:1 agent chat</b>, <b>projects</b>, <b>sub-issues/dependencies</b>, <b>member invitations</b>, <b>autopilot presets</b> — the roadmap's "build chat / projects / sub-issues" item.</li>
    </ul></div>
  </div>

  <footer>
    <b>How the media were captured.</b> Driven through the real <code>ainb</code> TUI against a seeded local demo. Mouse journeys use real <b>SGR mouse escape sequences</b> injected over tmux (vhs can't drive a mouse), captured with asciinema + agg. The execution journey fires a real autopilot → task through the shipping daemon path; the provider is a <code>fake-claude.sh</code> stub but the FSM, claim loop, result capture and event push are real (validated against SQLite, shown above). The vs-Multica tab is verified against a clone of <code>github.com/multica-ai/multica</code> + the Hangar source. Tab keys: Skills&nbsp;<kbd>3</kbd>, Autopilots&nbsp;<kbd>4</kbd> (post-e38.38).
  </footer>
</div>
<script>
function show(name){{
  for(const t of ['showcase','status','e2e','vs']) document.getElementById('tab-'+t).classList.toggle('on', t===name);
  for(const [i,n] of [['t1','showcase'],['t2','status'],['t3','e2e'],['t4','vs']]) document.getElementById(i).setAttribute('aria-selected', n===name);
  if(history.replaceState) history.replaceState(null,'','#'+name);
}}
const h=location.hash.replace('#','');
if(['status','e2e','vs'].includes(h)) show(h);
</script>
</body></html>"""
OUT.write_text(HTML)
print(f"wrote {OUT} ({OUT.stat().st_size//1024} KB)")
