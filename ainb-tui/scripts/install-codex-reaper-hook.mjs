#!/usr/bin/env node
// Idempotent installer for the repo-owned codex-orphan reaper as a Claude Code
// SessionStart hook in ~/.claude/settings.json. It mutates only
// hooks.SessionStart, preserving every other key and hook, and backs the file
// up before any write.
//
//   node install-codex-reaper-hook.mjs              # install / upgrade
//   node install-codex-reaper-hook.mjs --dry-run    # preview install
//   node install-codex-reaper-hook.mjs --uninstall  # remove the hook
//   node install-codex-reaper-hook.mjs --uninstall --dry-run
//
// This installs a LOCAL user-scope hook. It does NOT modify the vendored
// openai-codex plugin (which is overwritten on update); the hook is a backstop
// that survives plugin updates.
//
// The installed command is written to survive the repo being moved, renamed, or
// deleted: `test -f "<abs>" && node "<abs>" || true`. If the script is gone the
// hook is a silent no-op instead of a "Cannot find module" error printed on
// every session start.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const SETTINGS_PATH = path.join(os.homedir(), ".claude", "settings.json");
const HOOK_MARKER = "reap-codex-orphans.mjs";
const HOOK_TIMEOUT_SECONDS = 5;

// The reaper lives next to this installer.
const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const reaperAbsPath = path.join(scriptDir, "reap-codex-orphans.mjs");
// Resilient: a missing script -> `test -f` fails -> `|| true` keeps the hook a
// no-op that never fails or noises the session.
const hookCommand = `test -f "${reaperAbsPath}" && node "${reaperAbsPath}" || true`;

function readSettings() {
  try {
    const raw = fs.readFileSync(SETTINGS_PATH, "utf8");
    return { json: JSON.parse(raw), existed: true };
  } catch (err) {
    if (err.code === "ENOENT") {
      return { json: {}, existed: false };
    }
    throw err;
  }
}

// The reaper's command string inside an entry, or null if the entry has none.
function reaperCommandOf(entry) {
  if (!entry || !Array.isArray(entry.hooks)) {
    return null;
  }
  const hook = entry.hooks.find(
    (h) => h && typeof h.command === "string" && h.command.includes(HOOK_MARKER),
  );
  return hook ? hook.command : null;
}

function buildReaperEntry() {
  return {
    hooks: [
      {
        type: "command",
        command: hookCommand,
        timeout: HOOK_TIMEOUT_SECONDS,
      },
    ],
  };
}

// Plan an install: append if absent, replace if a reaper entry exists with a
// different command (upgrade), no-op if identical.
function planInstall(sessionStart) {
  const idx = sessionStart.findIndex((e) => reaperCommandOf(e) !== null);
  if (idx === -1) {
    return { next: [...sessionStart, buildReaperEntry()], action: "append" };
  }
  if (reaperCommandOf(sessionStart[idx]) === hookCommand) {
    return { next: sessionStart, action: "noop" };
  }
  const next = sessionStart.slice();
  next[idx] = buildReaperEntry();
  return { next, action: "replace" };
}

// Plan an uninstall: drop every entry that references the reaper.
function planUninstall(sessionStart) {
  const next = sessionStart.filter((e) => reaperCommandOf(e) === null);
  return { next, removed: sessionStart.length - next.length };
}

// Back up current settings without clobbering a differing prior backup.
function writeBackup() {
  const current = fs.readFileSync(SETTINGS_PATH, "utf8");
  const primary = `${SETTINGS_PATH}.bak`;
  if (fs.existsSync(primary)) {
    if (fs.readFileSync(primary, "utf8") === current) {
      return primary; // identical backup already present
    }
    const alt = `${SETTINGS_PATH}.bak-${Date.now()}`;
    fs.writeFileSync(alt, current, "utf8");
    return alt;
  }
  fs.writeFileSync(primary, current, "utf8");
  return primary;
}

function main() {
  const dryRun = process.argv.includes("--dry-run");
  const uninstall = process.argv.includes("--uninstall");
  const { json, existed } = readSettings();

  const hooks =
    json.hooks && typeof json.hooks === "object" && !Array.isArray(json.hooks)
      ? json.hooks
      : {};
  const sessionStart = Array.isArray(hooks.SessionStart)
    ? hooks.SessionStart
    : [];
  const before = JSON.stringify(sessionStart, null, 2);

  console.log(
    `Settings file: ${SETTINGS_PATH}${existed ? "" : " (does not exist; would be created)"}`,
  );
  console.log(`Reaper script: ${reaperAbsPath}`);
  console.log(`Mode: ${uninstall ? "uninstall" : "install"}${dryRun ? " (dry-run)" : ""}`);

  let plan;
  let summary;
  if (uninstall) {
    plan = planUninstall(sessionStart);
    if (plan.removed === 0) {
      console.log(
        "\nNo reaper hook present in SessionStart -> nothing to remove (idempotent).",
      );
      console.log(`\n--- SessionStart (unchanged) ---\n${before}`);
      return;
    }
    summary = `Removed ${plan.removed} reaper hook entr${plan.removed === 1 ? "y" : "ies"} from SessionStart.`;
  } else {
    plan = planInstall(sessionStart);
    if (plan.action === "noop") {
      console.log(
        "\nReaper hook already present with the current command -> nothing to do (idempotent).",
      );
      console.log(`\n--- SessionStart (unchanged) ---\n${before}`);
      return;
    }
    summary =
      plan.action === "replace"
        ? "Upgraded the existing reaper hook command in SessionStart."
        : "Added the reaper hook to SessionStart.";
  }

  const after = JSON.stringify(plan.next, null, 2);
  console.log(`\n--- SessionStart BEFORE ---\n${before}`);
  console.log(`\n--- SessionStart AFTER ---\n${after}`);

  if (dryRun) {
    console.log(`\n[dry-run] ${summary} No changes written.`);
    return;
  }

  const backupPath = existed ? writeBackup() : null;
  const nextJson = {
    ...json,
    hooks: { ...hooks, SessionStart: plan.next },
  };
  fs.writeFileSync(
    SETTINGS_PATH,
    `${JSON.stringify(nextJson, null, 2)}\n`,
    "utf8",
  );

  console.log(
    `\n${summary}\nWrote ${SETTINGS_PATH}. Backup: ${backupPath ?? "(none; settings did not previously exist)"}`,
  );
}

try {
  main();
} catch (err) {
  console.error(`install-codex-reaper-hook failed: ${err.message}`);
  process.exit(1);
}
