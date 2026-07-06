// L2 — parity guard. The SES sandbox can't import the lib, so jarvis.js
// inlines a verbatim copy of buildPanel. This asserts the two never drift.
//   node --test parity.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'

const here = dirname(fileURLToPath(import.meta.url))

function extractParityBlock(path) {
  const src = readFileSync(join(here, path), 'utf8')
  const start = src.indexOf('// PARITY-START')
  const end = src.indexOf('// PARITY-END')
  assert.ok(start !== -1, `PARITY-START marker missing in ${path}`)
  assert.ok(end !== -1, `PARITY-END marker missing in ${path}`)
  assert.ok(end > start, `PARITY markers out of order in ${path}`)
  return src
    .slice(start + '// PARITY-START'.length, end)
    .split('\n')
    .map((l) => l.replace(/\s+$/, ''))
    .join('\n')
    .trim()
}

test('buildPanel is byte-identical in lib and workflow', () => {
  const lib = extractParityBlock('hangar.logic.mjs')
  const wf = extractParityBlock('jarvis.js')
  assert.equal(
    wf,
    lib,
    'jarvis.js inline buildPanel drifted from hangar.logic.mjs — re-sync the PARITY block in both files',
  )
})

test('jarvis starts with the required meta export', () => {
  const wf = readFileSync(join(here, 'jarvis.js'), 'utf8')
  assert.match(wf.trimStart(), /^export const meta = \{/)
})

test('jarvis.meta.name is "jarvis"', () => {
  const wf = readFileSync(join(here, 'jarvis.js'), 'utf8')
  assert.match(wf, /name:\s*'jarvis'/)
})
