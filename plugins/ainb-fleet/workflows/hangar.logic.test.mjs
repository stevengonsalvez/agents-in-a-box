// L1 — pure builder tests (GATE 1: shape + sort). Deterministic, no runtime.
//   node --test hangar.logic.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { buildPanel, PRIORITY } from './hangar.logic.mjs'

const ask = {
  row: {
    session: { id: 'a1', tmux_session: 'tmux_scratch-ask', workspace_name: 'scratch' },
    kind: 'ASK',
    context: {
      header: 'Approve lineup',
      question: 'approve bead shape?',
      options: [
        { label: 'Approve as-is', description: 'create all' },
        { label: 'Skip B21', description: 'fewer beads' },
        { label: 'Split schema', description: 'more atomic' },
      ],
      multi_select: false,
    },
    route_hint: 'broker',
  },
  enriched: { line: '24-bead epic ready; approve shape', suggestion: 'Approve as-is' },
}
const err = {
  row: {
    session: { tmux_session: 'tmux_scratch-err' },
    kind: 'ERR',
    context: { pattern: 'rate_limited' },
    route_hint: 'tmux',
  },
  enriched: { line: '3rd 429 in 2m', suggestion: 'retry' },
}
const idle = {
  row: {
    session: { tmux_session: 'tmux_scratch-idle' },
    kind: 'IDLE',
    context: { idle_minutes: 17 },
    route_hint: 'tmux',
  },
  enriched: { line: 'idle 17m, work complete', suggestion: 'close' },
}
const wait = {
  row: {
    session: { tmux_session: 'tmux_scratch-wait' },
    kind: 'WAIT',
    context: { marker: 'WAITING:', text: 'need a token' },
    route_hint: 'broker',
  },
  enriched: { line: 'awaiting token', suggestion: '' },
}

test('3-mixed: banner counts correct', () => {
  const { banner } = buildPanel([idle, ask, err])
  assert.equal(banner.need, 3)
  assert.equal(banner.ask, 1)
  assert.equal(banner.err, 1)
  assert.equal(banner.idle, 1)
})

test('3-mixed: priority sort ASK>ERR>IDLE', () => {
  const { cards, banner } = buildPanel([idle, ask, err])
  assert.equal(cards[0].kind, 'ASK')
  assert.equal(cards[1].kind, 'ERR')
  assert.equal(cards[2].kind, 'IDLE')
  assert.equal(banner.top.kind, 'ASK')
})

test('full priority order ASK>ERR>IDLE>WAIT', () => {
  const { cards } = buildPanel([wait, idle, err, ask])
  assert.deepEqual(cards.map((c) => c.kind), ['ASK', 'ERR', 'IDLE', 'WAIT'])
  assert.deepEqual(Object.values(PRIORITY), [0, 1, 2, 3])
})

test('ASK card carries emoji, line, enriched, options[]', () => {
  const { cards } = buildPanel([ask])
  assert.equal(cards[0].emoji, '\u{1F7E1}')
  assert.equal(cards[0].enriched, 'Approve as-is')
  assert.deepEqual(cards[0].options, ['Approve as-is', 'Skip B21', 'Split schema'])
})

test('non-ASK cards have no options key', () => {
  const { cards } = buildPanel([err, idle])
  assert.equal(cards[0].options, undefined)
  assert.equal(cards[1].options, undefined)
})

test('card line falls back to context when enrich missing', () => {
  const bare = { row: { session: { tmux_session: 't' }, kind: 'ERR', context: { pattern: 'overloaded' } }, enriched: null }
  const { cards } = buildPanel([bare])
  assert.equal(cards[0].line, 'overloaded')
})

test('ASK ask maps options 1:1 with label+description', () => {
  const { asks } = buildPanel([ask])
  assert.equal(asks[0].question, 'Approve lineup: approve bead shape?')
  assert.equal(asks[0].options.length, 3)
  assert.deepEqual(asks[0].options[0], { label: 'Approve as-is', description: 'create all' })
  assert.equal(asks[0].route.target, 'tmux_scratch-ask')
  assert.equal(asks[0].route.hint, 'broker')
})

test('ERR ask offers Retry/Skip/Investigate', () => {
  const { asks } = buildPanel([err])
  assert.deepEqual(asks[0].options.map((o) => o.label), ['Retry', 'Skip', 'Investigate'])
})

test('IDLE ask offers Resume/Close', () => {
  const { asks } = buildPanel([idle])
  assert.match(asks[0].question, /idle 17m/)
  assert.deepEqual(asks[0].options.map((o) => o.label), ['Resume', 'Close'])
})

test('WAIT ask surfaces the marker text', () => {
  const { asks } = buildPanel([wait])
  assert.match(asks[0].question, /need a token/)
})

test('0-session: empty panel, banner.need=0, top=null', () => {
  const { banner, cards, asks } = buildPanel([])
  assert.equal(banner.need, 0)
  assert.equal(banner.top, null)
  assert.deepEqual(cards, [])
  assert.deepEqual(asks, [])
})

test('null/garbage rows filtered out', () => {
  const { banner } = buildPanel([null, ask, undefined, false])
  assert.equal(banner.need, 1)
})

test('unknown kind sorts last, session falls back to workspace_name', () => {
  const weird = { row: { session: { workspace_name: 'ws-only' }, kind: 'MYSTERY', context: {} }, enriched: null }
  const { cards } = buildPanel([weird, ask])
  assert.equal(cards[0].kind, 'ASK')
  assert.equal(cards[1].session, 'ws-only')
})
