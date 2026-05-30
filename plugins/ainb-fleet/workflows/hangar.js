export const meta = {
  name: 'hangar',
  description: 'Multi-verb fleet orchestrator. One deterministic workflow that dispatches by args.verb. Verbs today: `needs` (the Jarvis panel — discover → enrich → prioritize → render-ready cards), `standup` (per-workspace briefing — discover → enrich → group; pass brief:false for a cheap raw roster), `sequence` (ordered ack-gated multi-step send via pipeline). Reads always go through ainb (which tails JSONL + pane); writes always happen in the calling session (tmux send-keys), never inside the sandbox. Enrich model configurable via args.enrichModel (default haiku).',
  whenToUse: 'Invoked by the per-verb skills under ainb-fleet (fleet-needs / standup-rich / sequence). Args = {verb, ...opts}. Default verb = "needs" when omitted.',
  phases: [
    { title: 'Discover',   detail: 'ainb fleet <verb> --json (Rust does the JSONL read)' },
    { title: 'Enrich',     detail: 'parallel enrich per session (model configurable, default haiku) — needs + standup' },
    { title: 'Prioritize', detail: 'sort + render-ready {banner,cards,asks} — needs verb only' },
    { title: 'Group',      detail: 'group briefing by workspace — standup verb only' },
    { title: 'Step',       detail: 'per-step send-then-ack — sequence verb only' },
  ],
}

// ---------------------------------------------------------------------------
// Verb dispatch
// ---------------------------------------------------------------------------
const parsed = (() => {
  if (typeof args === 'object' && args !== null) return args
  if (typeof args === 'string' && args.trim().length > 0) return { verb: args.trim() }
  return { verb: 'needs' }
})()
const verb = parsed.verb || 'needs'

log('hangar verb=' + verb)

if (verb === 'needs') {
  return await runNeeds(parsed)
}
if (verb === 'standup') {
  return await runStandup(parsed)
}
if (verb === 'sequence') {
  return await runSequence(parsed)
}
throw new Error('hangar: unknown verb "' + verb + '" — valid: needs | standup | sequence')

// ===========================================================================
// VERB: needs — the Jarvis panel (former fleet-needs flow)
// ===========================================================================
async function runNeeds(opts) {
  // Enrich model is configurable via args.enrichModel; defaults to haiku
  // (cheap — one agent per blocked session, can fan out wide). Pass e.g.
  // {verb:'needs', enrichModel:'sonnet'} or 'opus' for richer suggestions.
  const enrichModel = (opts && typeof opts.enrichModel === 'string' && opts.enrichModel.trim()) || 'haiku'

  phase('Discover')

  const DISCOVER_SCHEMA = {
    type: 'object',
    required: ['json'],
    properties: { json: { type: 'string', description: 'raw verbatim stdout of `ainb --format json fleet needs`' } },
  }

  const discovered = await agent(
    'Run EXACTLY this command and return its raw stdout verbatim in the `json` field — ' +
      'no commentary, no markdown code fences, just the JSON the command printed:\n\n' +
      '    ainb --format json fleet needs\n\n' +
      'If the command errors, prints nothing, or there is no fleet, return json = "[]".',
    { label: 'discover', phase: 'Discover', schema: DISCOVER_SCHEMA },
  )

  let sessions = []
  try {
    sessions = JSON.parse(discovered.json)
  } catch (_e) {
    sessions = []
  }
  if (!Array.isArray(sessions)) sessions = []

  // post-Discover kind breakdown
  const kCounts = sessions.reduce((acc, s) => { const k = (s && s.kind) || 'WAIT'; acc[k] = (acc[k] || 0) + 1; return acc }, {})
  log('discovered ' + sessions.length + ' · ASK:' + (kCounts.ASK || 0) + ' ERR:' + (kCounts.ERR || 0) + ' IDLE:' + (kCounts.IDLE || 0) + ' WAIT:' + (kCounts.WAIT || 0))

  phase('Enrich')
  log('enrich model: ' + enrichModel)

  const ENRICH_SCHEMA = {
    type: 'object',
    required: ['line', 'suggestion'],
    properties: {
      line: { type: 'string', description: 'one terse operator-facing summary, <= 12 words' },
      suggestion: { type: 'string', description: 'ASK: best option label verbatim; ERR: retry|skip|investigate; IDLE: resume|close; else empty' },
    },
  }

  // Label helpers: rich agent labels in the /workflows monitor.
  const tagOf = (kind, ctx) => {
    if (kind === 'IDLE' && ctx && typeof ctx.idle_minutes === 'number') return '[IDLE ' + ctx.idle_minutes + 'm]'
    return '[' + (kind || 'WAIT') + ']'
  }
  const idOf = (sess, idx) => {
    const tm = sess.tmux_session || sess.workspace_name
    if (tm) return tm
    const cwd = sess.cwd || ''
    const segs = cwd.split('/').filter(Boolean)
    const tail = (segs[segs.length - 1] || '').slice(0, 30)
    const peer = sess.peer_id || ''
    const skipTails = new Set(['tmp', 'private', 'home', 'Users'])
    if (tail && !skipTails.has(tail)) return peer ? tail + ':' + peer : tail
    return peer || ('session-' + idx)
  }

  let done = 0
  const total = sessions.length
  const enriched = await parallel(
    sessions.map((s, i) => () => {
      const kind = (s && s.kind) || 'WAIT'
      const ctx = (s && s.context) || {}
      const sess = (s && s.session) || {}
      const sid = sess.id || idOf(sess, i)
      const ctxStr = JSON.stringify(ctx).slice(0, 900)
      const label = tagOf(kind, ctx) + ' enrich:' + idOf(sess, i) + '  route:' + (s.route_hint || 'tmux')
      const onDone = () => {
        done++
        if (done % 5 === 0 || done === total) log('enriched ' + done + '/' + total)
      }
      return agent(
        'A claude session is blocked.\n' +
          'session_id=' + sid + ' kind=' + kind + '\n' +
          'context=' + ctxStr + '\n\n' +
          'Return `line`: one terse operator-facing summary (<=12 words) of what this session needs.\n' +
          'Return `suggestion`: for ASK, the single best option label verbatim from the options; ' +
          'for ERR, one of retry|skip|investigate; for IDLE, one of resume|close; otherwise empty string.',
        { label: label, phase: 'Enrich', model: enrichModel, schema: ENRICH_SCHEMA },
      )
        .then((e) => { onDone(); return { row: s, enriched: e } })
        .catch(() => { onDone(); return { row: s, enriched: null } })
    }),
  )

  phase('Prioritize')

  const panel = buildPanel(enriched)

  // post-Prioritize: top ASK picks digest
  const topPicks = panel.cards
    .filter((c) => c.kind === 'ASK' && c.enriched)
    .slice(0, 5)
    .map((c) => c.session.replace(/^tmux_/, '').slice(0, 18) + '=' + c.enriched.slice(0, 30))
    .join(' · ')
  if (topPicks) log('top picks: ' + topPicks)
  log('prioritized ' + panel.cards.length + ' card(s), ' + panel.asks.length + ' ask(s)')

  return { verb: 'needs', ...panel }
}

// ===========================================================================
// VERB: standup — raw fleet listing
// ===========================================================================
async function runStandup(opts) {
  phase('Discover')

  const STANDUP_SCHEMA = {
    type: 'object',
    required: ['json'],
    properties: { json: { type: 'string', description: 'raw stdout of `ainb --format json fleet standup`' } },
  }

  const filterArg = opts && typeof opts.filter === 'string' ? (' (note: caller wants to filter on "' + opts.filter + '" client-side)') : ''

  const discovered = await agent(
    'Run EXACTLY: ainb --format json fleet standup\nReturn raw stdout verbatim in the `json` field. ' +
      'No commentary, no markdown fences. If empty, return json = "[]".' + filterArg,
    { label: 'standup', phase: 'Discover', schema: STANDUP_SCHEMA },
  )

  let sessions = []
  try {
    sessions = JSON.parse(discovered.json)
  } catch (_e) {
    sessions = []
  }
  if (!Array.isArray(sessions)) sessions = []

  if (opts && typeof opts.filter === 'string' && opts.filter.length > 0) {
    const needle = opts.filter.toLowerCase()
    sessions = sessions.filter((s) => {
      const tmux = (s.tmux_session || '').toLowerCase()
      const ws = (s.workspace_name || '').toLowerCase()
      const cwd = (s.cwd || '').toLowerCase()
      return tmux.includes(needle) || ws.includes(needle) || cwd.includes(needle)
    })
  }

  log('standup: ' + sessions.length + ' session(s)' + (filterArg ? ' after filter' : ''))

  // Thin mode (opts.brief === false): raw roster, no enrich — cheap, 1 agent.
  if (opts && opts.brief === false) {
    return { verb: 'standup', mode: 'roster', count: sessions.length, sessions }
  }

  // ---- Enrich: one agent per session drafts a real "what it's doing" line ----
  const enrichModel = (opts && typeof opts.enrichModel === 'string' && opts.enrichModel.trim()) || 'haiku'

  phase('Enrich')
  log('enrich model: ' + enrichModel)

  const STANDUP_ENRICH_SCHEMA = {
    type: 'object',
    required: ['activity', 'state', 'stale'],
    properties: {
      activity: { type: 'string', description: '<=10 words: what this session is working on, or idle/done' },
      state: { type: 'string', enum: ['active', 'idle', 'done', 'stuck'] },
      stale: { type: 'string', description: 'human relative age from last_seen_ms vs now, e.g. "3h", "20m", "2d"' },
    },
  }

  let done = 0
  const total = sessions.length
  const enriched = await parallel(
    sessions.map((s, i) => () => {
      const ws = s.workspace_name || 'unknown'
      const tmux = s.tmux_session || s.bg_job_id || ('session-' + i)
      const cwd = s.cwd || ''
      const onDone = () => { done++; if (done % 10 === 0 || done === total) log('briefed ' + done + '/' + total) }
      return agent(
        'A claude session. workspace=' + ws + ' tmux=' + tmux + ' cwd=' + cwd +
          ' last_seen_ms=' + (s.last_seen_ms || 0) + ' raw_summary=' + JSON.stringify(s.summary || '') + '\n\n' +
          'Determine what this session is actually doing. Read its recent transcript tail — try:\n' +
          '  slug=$(echo "' + cwd + '" | sed "s#/#-#g"); f=$(ls -t ~/.claude/projects/$slug/*.jsonl 2>/dev/null | head -1); tail -n 40 "$f"\n' +
          'If the tail is unreadable, fall back to the raw_summary.\n\n' +
          'Return `activity`: <=10 words on what it is working on (or "idle"/"done" if nothing active — ignore noise like "No response requested").\n' +
          'Return `state`: active | idle | done | stuck.\n' +
          'Return `stale`: relative age computed from last_seen_ms vs the current time (run `date +%s%3N` if needed), e.g. "3h", "20m", "2d".',
        { label: ws + ':' + tmux, phase: 'Enrich', model: enrichModel, schema: STANDUP_ENRICH_SCHEMA },
      )
        .then((e) => { onDone(); return { s, e } })
        .catch(() => { onDone(); return { s, e: null } })
    }),
  )

  // ---- Group: pure JS, by workspace ----
  phase('Group')

  const byWs = new Map()
  for (const { s, e } of enriched.filter(Boolean)) {
    const ws = s.workspace_name || 'unknown'
    if (!byWs.has(ws)) byWs.set(ws, [])
    byWs.get(ws).push({
      tmux: s.tmux_session || s.bg_job_id || 'unknown',
      activity: (e && e.activity) || (s.summary || '').slice(0, 60) || 'unknown',
      state: (e && e.state) || 'idle',
      stale: (e && e.stale) || '?',
      routable: s.peer_id ? 'broker' : (s.tmux_session ? 'tmux' : 'none'),
    })
  }

  const groups = Array.from(byWs.entries())
    .map(([workspace, sess]) => ({ workspace, count: sess.length, sessions: sess }))
    .sort((a, b) => b.count - a.count)

  const stateCounts = enriched.filter(Boolean).reduce((acc, { e }) => {
    const st = (e && e.state) || 'idle'; acc[st] = (acc[st] || 0) + 1; return acc
  }, {})
  log('briefing: ' + sessions.length + ' across ' + groups.length + ' workspace(s) · ' +
    'active:' + (stateCounts.active || 0) + ' idle:' + (stateCounts.idle || 0) +
    ' done:' + (stateCounts.done || 0) + ' stuck:' + (stateCounts.stuck || 0))

  return {
    verb: 'standup',
    mode: 'briefing',
    count: sessions.length,
    states: stateCounts,
    groups,
  }
}

// ===========================================================================
// VERB: sequence — ordered ack-gated multi-step send (skeleton, Phase 2 of spec)
// ===========================================================================
async function runSequence(opts) {
  const steps = (opts && Array.isArray(opts.steps)) ? opts.steps : []
  const filter = (opts && typeof opts.filter === 'string') ? opts.filter : ''
  if (steps.length === 0) {
    throw new Error('hangar(sequence): args.steps must be a non-empty array of prompts')
  }
  if (!filter) {
    throw new Error('hangar(sequence): args.filter is required (regex against tmux/workspace name)')
  }

  // pipeline: send → ack. Each step proceeds only after the previous one's ack lands.
  const results = []
  for (let i = 0; i < steps.length; i++) {
    phase('Step')

    const SEQ_SCHEMA = {
      type: 'object',
      required: ['sent', 'acked'],
      properties: {
        sent:  { type: 'array', items: { type: 'string' } },
        acked: { type: 'array', items: { type: 'string' } },
        timed_out: { type: 'array', items: { type: 'string' } },
      },
    }

    const step = steps[i]
    const stepIdx = i + 1
    const r = await agent(
      'Run EXACTLY:\n' +
        '    ainb fleet sequence "' + String(step).replace(/"/g, '\\"') + '" --filter "' + filter + '" --timeout 60\n\n' +
        'Capture which targets sent vs acked. Return sent[], acked[] (tmux_session names), and timed_out[] if any.',
      { label: 'step:' + stepIdx, phase: 'Step', schema: SEQ_SCHEMA },
    )
    log('sequence step ' + stepIdx + '/' + steps.length + ': sent=' + (r.sent?.length ?? 0) + ' acked=' + (r.acked?.length ?? 0))
    results.push({ step: stepIdx, prompt: step, ...r })
  }

  return { verb: 'sequence', filter, total_steps: steps.length, results }
}

// ===========================================================================
// PURE BUILDER — verbatim copy of fleet-needs.logic.mjs (parity-guarded)
// ===========================================================================

// PARITY-START
function buildPanel(enriched) {
  const PRIORITY = { ASK: 0, ERR: 1, IDLE: 2, WAIT: 3 }
  const EMOJI = { ASK: '\u{1F7E1}', ERR: '\u{1F534}', IDLE: '\u{26AA}', WAIT: '\u{1F7E2}' }

  const rows = (enriched || []).filter(Boolean)
  rows.sort((a, b) => (PRIORITY[a.row && a.row.kind] ?? 9) - (PRIORITY[b.row && b.row.kind] ?? 9))

  const tmuxOf = (r) => {
    const sess = (r.row && r.row.session) || {}
    return sess.tmux_session || sess.workspace_name || 'unknown'
  }

  const banner = {
    need: rows.length,
    err: rows.filter((r) => r.row.kind === 'ERR').length,
    ask: rows.filter((r) => r.row.kind === 'ASK').length,
    idle: rows.filter((r) => r.row.kind === 'IDLE').length,
    wait: rows.filter((r) => r.row.kind === 'WAIT').length,
    top: rows[0] ? { session: tmuxOf(rows[0]), kind: rows[0].row.kind } : null,
  }

  const cards = rows.map((r) => {
    const kind = r.row.kind
    const ctx = r.row.context || {}
    const en = r.enriched || {}
    let line = en.line || ''
    if (!line) {
      if (kind === 'ASK') line = ctx.question || 'question'
      else if (kind === 'ERR') line = ctx.pattern || 'error'
      else if (kind === 'IDLE') line = 'idle ' + (ctx.idle_minutes ?? '?') + 'm'
      else line = ctx.text || 'waiting'
    }
    return {
      emoji: EMOJI[kind] || '•',
      kind,
      session: tmuxOf(r),
      line,
      enriched: en.suggestion || null,
      options: kind === 'ASK' ? (ctx.options || []).map((o) => o.label) : undefined,
    }
  })

  const asks = rows.map((r) => {
    const kind = r.row.kind
    const ctx = r.row.context || {}
    const en = r.enriched || {}
    const tmux = tmuxOf(r)
    const route = { target: tmux, hint: r.row.route_hint || 'tmux' }
    const header = tmux.slice(0, 12)
    if (kind === 'ASK') {
      return {
        question: (ctx.header ? ctx.header + ': ' : '') + (ctx.question || 'needs input'),
        header,
        options: (ctx.options || []).map((o) => ({ label: o.label, description: o.description || '' })),
        multiSelect: !!ctx.multi_select,
        suggestion: en.suggestion || null,
        route,
      }
    }
    if (kind === 'ERR') {
      return {
        question: tmux + ' hit `' + (ctx.pattern || 'error') + '` — what now?',
        header,
        options: [
          { label: 'Retry', description: 'send continue' },
          { label: 'Skip', description: 'leave it' },
          { label: 'Investigate', description: 'open the session' },
        ],
        multiSelect: false,
        suggestion: en.suggestion || null,
        route,
      }
    }
    if (kind === 'IDLE') {
      return {
        question: tmux + ' idle ' + (ctx.idle_minutes ?? '?') + 'm — resume?',
        header,
        options: [
          { label: 'Resume', description: 'send continue' },
          { label: 'Close', description: 'leave idle' },
        ],
        multiSelect: false,
        suggestion: en.suggestion || null,
        route,
      }
    }
    return {
      question: tmux + ' says: ' + (ctx.text || 'waiting'),
      header,
      options: [{ label: 'Acknowledge', description: 'respond' }],
      multiSelect: false,
      suggestion: en.suggestion || null,
      route,
    }
  })

  return { banner, cards, asks }
}
// PARITY-END
