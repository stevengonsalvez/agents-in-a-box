#!/usr/bin/env node
// PreCompact hook: re-inject full caveman ruleset before context compaction.
// Output is included in the compacted summary so rules survive compaction.
// If caveman is off, emits nothing.

const fs = require('fs');
const path = require('path');
const os = require('os');

const claudeDir = process.env.CLAUDE_CONFIG_DIR || path.join(os.homedir(), '.claude');
const flagPath = path.join(claudeDir, '.caveman-active');
const VALID = new Set(['off','lite','full','ultra','wenyan-lite','wenyan','wenyan-full','wenyan-ultra','commit','review','compress']);
const INDEPENDENT = new Set(['commit', 'review', 'compress']);

function readMode() {
  try {
    const st = fs.lstatSync(flagPath);
    if (st.isSymbolicLink() || !st.isFile() || st.size > 64) return null;
    const raw = fs.readFileSync(flagPath, 'utf8').trim().toLowerCase();
    return VALID.has(raw) && raw !== 'off' ? raw : null;
  } catch { return null; }
}

function findSkillMd() {
  // Try installed plugin cache (latest version)
  const cacheDir = path.join(claudeDir, 'plugins', 'cache', 'caveman', 'caveman');
  try {
    const versions = fs.readdirSync(cacheDir).sort().reverse();
    for (const ver of versions) {
      const p = path.join(cacheDir, ver, 'skills', 'caveman', 'SKILL.md');
      if (fs.existsSync(p)) return p;
    }
  } catch {}
  // Fallback to user skills dir
  const fallback = path.join(claudeDir, 'skills', 'caveman', 'SKILL.md');
  return fs.existsSync(fallback) ? fallback : null;
}

const mode = readMode();
if (!mode) process.exit(0);

if (INDEPENDENT.has(mode)) {
  process.stdout.write(
    `[POST-COMPACTION] CAVEMAN MODE STILL ACTIVE — level: ${mode}. ` +
    `Behavior defined by /caveman-${mode} skill.`
  );
  process.exit(0);
}

const modeLabel = mode === 'wenyan' ? 'wenyan-full' : mode;
const skillPath = findSkillMd();

let output;
if (skillPath) {
  try {
    const content = fs.readFileSync(skillPath, 'utf8');
    const body = content.replace(/^---[\s\S]*?---\s*/, '');
    const filtered = body.split('\n').reduce((acc, line) => {
      const tableRow = line.match(/^\|\s*\*\*(\S+?)\*\*\s*\|/);
      if (tableRow) {
        if (tableRow[1] === modeLabel) acc.push(line);
        return acc;
      }
      const example = line.match(/^- (\S+?):\s/);
      if (example) {
        if (example[1] === modeLabel) acc.push(line);
        return acc;
      }
      acc.push(line);
      return acc;
    }, []);
    output =
      `[POST-COMPACTION PERSIST] CAVEMAN MODE STILL ACTIVE — level: ${modeLabel}\n\n` +
      filtered.join('\n');
  } catch {
    output =
      `[POST-COMPACTION PERSIST] CAVEMAN MODE STILL ACTIVE — level: ${modeLabel}\n\n` +
      `Drop articles/filler/pleasantries/hedging. Fragments OK. Technical terms exact. ` +
      `Code/commits/security: write normal. "stop caveman" or "normal mode" to exit.`;
  }
} else {
  output =
    `[POST-COMPACTION PERSIST] CAVEMAN MODE STILL ACTIVE — level: ${modeLabel}\n\n` +
    `Drop articles/filler/pleasantries/hedging. Fragments OK. Technical terms exact. ` +
    `Code/commits/security: write normal. "stop caveman" or "normal mode" to exit.`;
}

process.stdout.write(output);
process.exit(0);
