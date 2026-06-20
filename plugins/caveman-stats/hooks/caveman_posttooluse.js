#!/usr/bin/env node
// PostToolUse hook: silently update .caveman-statusline-suffix on each tool use.
// Uses a watermark to read only new JSONL lines — fast on large session files.
// No stdout output — does not inject context.

const fs = require('fs');
const path = require('path');
const os = require('os');

const claudeDir = process.env.CLAUDE_CONFIG_DIR || path.join(os.homedir(), '.claude');

// Same compression ratios as caveman-stats.js
const COMPRESSION = { full: 0.65 };
const MODEL_PRICE = [
  ['claude-opus-4', 75], ['claude-sonnet-4', 15], ['claude-haiku-4', 4],
  ['claude-3-5-sonnet', 15], ['claude-3-5-haiku', 4], ['claude-3-opus', 75],
];

function humanize(n) {
  if (!Number.isFinite(n) || n <= 0) return '0';
  if (n >= 1e6) return (n / 1e6).toFixed(1) + 'M';
  if (n >= 1e3) return (n / 1e3).toFixed(1) + 'k';
  return String(Math.round(n));
}

function readMode() {
  const flagPath = path.join(claudeDir, '.caveman-active');
  const VALID = new Set(['off','lite','full','ultra','wenyan-lite','wenyan','wenyan-full','wenyan-ultra','commit','review','compress']);
  try {
    const st = fs.lstatSync(flagPath);
    if (st.isSymbolicLink() || !st.isFile() || st.size > 64) return null;
    const raw = fs.readFileSync(flagPath, 'utf8').trim().toLowerCase();
    return VALID.has(raw) ? raw : null;
  } catch { return null; }
}

function findSessionFile(sessionId) {
  if (!sessionId) return null;
  // Check cached path
  const cacheFile = path.join(claudeDir, `.caveman-sess-${sessionId.slice(0, 16)}`);
  try {
    const cached = fs.readFileSync(cacheFile, 'utf8').trim();
    if (fs.existsSync(cached)) return cached;
  } catch {}

  // Find by session ID in projects dir
  const projectsDir = path.join(claudeDir, 'projects');
  const target = `${sessionId}.jsonl`;
  const stack = [projectsDir];
  while (stack.length) {
    const dir = stack.pop();
    try {
      for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
        const full = path.join(dir, entry.name);
        if (entry.isDirectory()) { stack.push(full); continue; }
        if (entry.name === target) {
          try { fs.writeFileSync(cacheFile, full, { mode: 0o600 }); } catch {}
          return full;
        }
      }
    } catch {}
  }
  return null;
}

// Watermark: track last byte offset + running token sum per session
function readWatermark(sessionId) {
  const wmFile = path.join(claudeDir, `.caveman-wm-${sessionId.slice(0, 16)}`);
  try {
    const st = fs.lstatSync(wmFile);
    if (st.isSymbolicLink() || !st.isFile() || st.size > 256) return null;
    return JSON.parse(fs.readFileSync(wmFile, 'utf8'));
  } catch { return null; }
}

function writeWatermark(sessionId, data) {
  const wmFile = path.join(claudeDir, `.caveman-wm-${sessionId.slice(0, 16)}`);
  // Refuse symlinks
  try { if (fs.lstatSync(wmFile).isSymbolicLink()) return; } catch {}
  try { fs.writeFileSync(wmFile, JSON.stringify(data), { mode: 0o600 }); } catch {}
}

function countNewTokens(sessionFile, fromOffset) {
  let fd, output = 0, model = null, newOffset = fromOffset;
  try {
    const st = fs.statSync(sessionFile);
    if (st.size <= fromOffset) return { output: 0, model: null, offset: fromOffset };
    const buf = Buffer.alloc(st.size - fromOffset);
    fd = fs.openSync(sessionFile, 'r');
    const read = fs.readSync(fd, buf, 0, buf.length, fromOffset);
    newOffset = fromOffset + read;
    // Process complete lines only
    const text = buf.slice(0, read).toString('utf8');
    const lines = text.split('\n');
    for (const line of lines) {
      if (!line.trim()) continue;
      try {
        const e = JSON.parse(line);
        if (e.type !== 'assistant' || !e.message?.usage) continue;
        output += e.message.usage.output_tokens || 0;
        if (!model && e.message.model) model = e.message.model;
      } catch {}
    }
  } catch {} finally {
    if (fd !== undefined) try { fs.closeSync(fd); } catch {}
  }
  return { output, model, offset: newOffset };
}

function writeSuffix(suffix) {
  const suffixPath = path.join(claudeDir, '.caveman-statusline-suffix');
  try { if (fs.lstatSync(suffixPath).isSymbolicLink()) return; } catch {}
  try { fs.writeFileSync(suffixPath, suffix, { mode: 0o600 }); } catch {}
}

// Main: read stdin for session_id + transcript_path
let input = '';
process.stdin.on('data', c => { input += c; });
process.stdin.on('end', () => {
  try {
    let data = {};
    try { data = JSON.parse(input); } catch {}

    const mode = readMode();
    const ratio = mode ? COMPRESSION[mode] : null;
    if (!ratio) process.exit(0);

    const sessionId = data.session_id || process.env.CLAUDE_SESSION_ID;
    if (!sessionId) process.exit(0);

    // Prefer transcript_path from hook data, fallback to search
    let sessionFile = data.transcript_path || findSessionFile(sessionId);
    if (!sessionFile) process.exit(0);

    const wm = readWatermark(sessionId) || { offset: 0, outputTokens: 0, model: null };
    const { output, model, offset } = countNewTokens(sessionFile, wm.offset);

    const totalOutput = wm.outputTokens + output;
    const resolvedModel = model || wm.model;

    writeWatermark(sessionId, { offset, outputTokens: totalOutput, model: resolvedModel });

    if (totalOutput > 0) {
      const estSaved = Math.round(totalOutput / (1 - ratio)) - totalOutput;
      const suffix = estSaved > 0 ? `⛏ ${humanize(estSaved)}` : '';
      writeSuffix(suffix);
    }
  } catch {}
  process.exit(0);
});
