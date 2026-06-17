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
// Resilience: retry an agent call on transient failure (API throttle shows as
// "subagent completed without calling StructuredOutput"). setTimeout is the
// only timing primitive available in the SES sandbox (Date.now/Math.random are
// blocked), so backoff is fixed-step, no jitter.
// ===========================================================================
async function withRetry(fn, label, attempts, baseDelayMs) {
  const n = attempts || 3
  const base = baseDelayMs || 1500
  let lastErr
  for (let i = 1; i <= n; i++) {
    try {
      return await fn()
    } catch (e) {
      lastErr = e
      if (i < n) {
        const msg = (e && e.message ? e.message : String(e)).slice(0, 80)
        log('retry ' + label + ' (' + i + '/' + (n - 1) + ') after: ' + msg)
        await new Promise((r) => setTimeout(r, base * i))
      }
    }
  }
  throw lastErr
}

// ===========================================================================
// Batched enrich — ONE agent drafts a `suggestion` for every stale card and
// persists each to the content cache via `ainb fleet enrich-cache put`, so the
// next read is a free cache hit. Replaces the old one-subagent-per-session
// fan-out. Returns a map of enrich_key -> suggestion (via the parity-guarded
// `enrichMapFromItems` defined at the bottom of this file).
// ===========================================================================
async function batchedEnrich(stale, model) {
  const BATCH_SCHEMA = {
    type: 'object',
    required: ['items'],
    properties: {
      items: {
        type: 'array',
        items: {
          type: 'object',
          required: ['enrich_key', 'suggestion'],
          properties: {
            enrich_key: { type: 'string' },
            suggestion: { type: 'string' },
          },
        },
      },
    },
  }
  const cards = stale.map((s) => ({
    enrich_key: s.enrich_key,
    kind: (s && s.kind) || 'WAIT',
    context: JSON.stringify((s && s.context) || {}).slice(0, 600),
  }))
  const res = await withRetry(() => agent(
    'Several claude sessions are blocked. For EACH card below, draft a `suggestion`:\n' +
      '  ASK  → the single best option label, verbatim from the options\n' +
      '  ERR  → one of: retry | skip | investigate\n' +
      '  IDLE → one of: resume | close\n' +
      '  WAIT/other → empty string\n\n' +
      'Cards (JSON array of {enrich_key, kind, context}):\n' + JSON.stringify(cards) + '\n\n' +
      'Then PERSIST each non-empty suggestion so the next read is free — run once per card:\n' +
      '  ainb fleet enrich-cache put --key <enrich_key> --suggestion "<suggestion>"\n\n' +
      'Return `items`: an array of {enrich_key, suggestion}, one entry per input card.',
    { label: 'enrich:batch(' + cards.length + ')', phase: 'Enrich', model, schema: BATCH_SCHEMA },
  ), 'enrich-batch', 2, 1000).catch(() => ({ items: [] }))
  return enrichMapFromItems(res && res.items)
}

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

  let discovered
  try {
    discovered = await withRetry(() => agent(
      'Run EXACTLY this command and return its raw stdout verbatim in the `json` field — ' +
        'no commentary, no markdown code fences, just the JSON the command printed:\n\n' +
        '    ainb --format json fleet needs\n\n' +
        'If the command errors, prints nothing, or there is no fleet, return json = "[]".',
      { label: 'discover', phase: 'Discover', schema: DISCOVER_SCHEMA },
    ), 'discover')
  } catch (e) {
    // Read genuinely failed (e.g. API throttle persisted across retries).
    // Surface it — do NOT collapse to a false "0 NEED YOU".
    const reason = (e && e.message ? e.message : String(e)).slice(0, 120)
    log('discover failed after retries: ' + reason)
    return {
      verb: 'needs',
      error: 'fleet read failed: ' + reason,
      banner: { need: 0, err: 0, ask: 0, idle: 0, wait: 0, top: null },
      cards: [],
      asks: [],
    }
  }

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

  // ONE batched enrich agent covers EVERY card that needs it (cache misses),
  // regardless of fleet size — instead of one subagent per session. Cache hits
  // already carry `enriched` from the Rust reader and cost nothing. A single
  // bad item in the batch just leaves that card snippet-only; the rest survive.
  const stale = sessions.filter((s) => s && s.need_enrich && s.enrich_key)
  log('enrich: ' + stale.length + ' stale / ' + sessions.length + ' (cache hits free)')
  const suggMap = stale.length > 0 ? await batchedEnrich(stale, enrichModel) : {}

  const enriched = sessions.map((s) => {
    const cached = s && typeof s.enriched === 'string' ? s.enriched : null
    const fresh = s && s.enrich_key ? suggMap[s.enrich_key] : null
    const suggestion = fresh && fresh.length > 0 ? fresh : cached
    return { row: s, enriched: suggestion ? { suggestion } : null }
  })

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

  let discovered
  try {
    discovered = await withRetry(() => agent(
      'Run EXACTLY: ainb --format json fleet standup\nReturn raw stdout verbatim in the `json` field. ' +
        'No commentary, no markdown fences. If empty, return json = "[]".' + filterArg,
      { label: 'standup', phase: 'Discover', schema: STANDUP_SCHEMA },
    ), 'standup-discover')
  } catch (e) {
    const reason = (e && e.message ? e.message : String(e)).slice(0, 120)
    log('standup discover failed after retries: ' + reason)
    return { verb: 'standup', error: 'fleet read failed: ' + reason, count: 0, groups: [] }
  }

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

  // ONE batched agent briefs the whole fleet (instead of one per session); it
  // may read each transcript tail itself. Items missing from the result just
  // fall back to the raw summary in the Group step.
  const STANDUP_BATCH_SCHEMA = {
    type: 'object',
    required: ['items'],
    properties: {
      items: {
        type: 'array',
        items: {
          type: 'object',
          required: ['id', 'activity', 'state', 'stale'],
          properties: {
            id: { type: 'string', description: 'the session id, echoed verbatim from input' },
            activity: { type: 'string', description: '<=10 words: what this session is working on, or idle/done' },
            state: { type: 'string', enum: ['active', 'idle', 'done', 'stuck'] },
            stale: { type: 'string', description: 'relative age from last_seen_ms vs now, e.g. "3h", "20m", "2d"' },
          },
        },
      },
    },
  }
  const idForStandup = (s, i) => s.tmux_session || s.bg_job_id || 'session-' + i
  const cards = sessions.map((s, i) => ({
    id: idForStandup(s, i),
    workspace: s.workspace_name || 'unknown',
    cwd: s.cwd || '',
    last_seen_ms: s.last_seen_ms || 0,
    summary: typeof s.summary === 'string' ? s.summary.slice(0, 80) : '',
  }))
  const res = await withRetry(() => agent(
    'Brief a fleet of claude sessions. For EACH session below, decide what it is doing.\n' +
      'You MAY read a session\'s recent transcript tail to decide:\n' +
      '  slug=$(echo "<cwd>" | sed "s#/#-#g"); f=$(ls -t ~/.claude/projects/$slug/*.jsonl 2>/dev/null | head -1); tail -n 40 "$f"\n' +
      'Compute `stale` as a relative age from last_seen_ms vs now (run `date +%s%3N`). Ignore noise like "No response requested".\n\n' +
      'Sessions (JSON array): ' + JSON.stringify(cards) + '\n\n' +
      'Return `items`: array of {id (echoed verbatim), activity (<=10 words), state (active|idle|done|stuck), stale}, one per session.',
    { label: 'standup:batch(' + cards.length + ')', phase: 'Enrich', model: enrichModel, schema: STANDUP_BATCH_SCHEMA },
  ), 'standup-batch', 2, 1000).catch(() => ({ items: [] }))
  const byId = {}
  for (const it of (res && res.items) || []) {
    if (it && typeof it.id === 'string') byId[it.id] = it
  }
  const enriched = sessions.map((s, i) => ({ s, e: byId[idForStandup(s, i)] || null }))
  log('briefed ' + sessions.length + ' session(s) in 1 batch')

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
      routable: s.tmux_session ? 'tmux' : (s.peer_id ? 'broker' : 'none'),
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

function enrichMapFromItems(items) {
  const map = {}
  for (const it of items || []) {
    if (it && typeof it.enrich_key === 'string' && it.enrich_key.length > 0) {
      map[it.enrich_key] = typeof it.suggestion === 'string' ? it.suggestion : ''
    }
  }
  return map
}
// PARITY-END
