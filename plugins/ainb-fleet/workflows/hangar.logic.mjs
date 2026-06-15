// Pure builder logic for hangar (verb=needs). SOURCE OF TRUTH.
//
// The SES workflow sandbox cannot `import` this module at runtime, so
// hangar.js inlines a VERBATIM copy of `buildPanel` between the
// PARITY markers. parity.test.mjs asserts the two copies never drift.
//
// Keep `buildPanel` pure: no agent()/phase()/log(), only data transform.

export const PRIORITY = { ASK: 0, ERR: 1, IDLE: 2, WAIT: 3 }
export const EMOJI = { ASK: '\u{1F7E1}', ERR: '\u{1F534}', IDLE: '\u{26AA}', WAIT: '\u{1F7E2}' }

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

export { buildPanel, enrichMapFromItems }
