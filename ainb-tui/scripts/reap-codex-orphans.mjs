#!/usr/bin/env node
// Backstop reaper for orphaned codex app-server broker processes and their
// stale temp dirs. The vendored openai-codex Claude Code plugin spawns a
// per-session broker (detached + unref'd) into a fresh `${os.tmpdir()}/cxc-XXXX`
// dir, and only cleans it up in its SessionEnd hook. That hook never runs on
// SIGKILL/crash/power-cut, and every socket path is unique so nothing ever
// adopts a survivor: leaks are permanent and additive. This script runs at
// Claude Code SessionStart as an update-safe backstop OUTSIDE the vendored
// plugin. It must never block or fail the session, so it swallows everything
// and always exits 0.
//
// Safety contract:
//   - Kill by EXACT pid only, never pkill/killall/wildcard.
//   - Never touch the desktop Codex/ChatGPT app-server: it lives at
//     `.../ChatGPT.app/Contents/Resources/codex` (no `bin/codex`) with ppid!=1.
//     The reap filter (ppid==1 AND argv has `bin/codex` AND `app-server`)
//     excludes it on both axes.
//   - "Cannot answer" liveness == unknown == leave it alone. A dir is removed
//     only when provably stale (a broker.pid whose process is gone).

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";

// Temp-dir name prefixes used by the broker. `cxc-` is the default passed to
// `createBrokerSessionDir` in the vendored lib/broker-lifecycle.mjs.
const BROKER_DIR_PREFIXES = ["cxc-"];
const SOCKET_FILE = "broker.sock";
const PID_FILE = "broker.pid";

// Bounds so a SessionStart hook stays well under its 5s timeout even if the
// machine has hundreds of leaked dirs to sweep.
const SOCKET_PROBE_MS = 150;
const SWEEP_CONCURRENCY = 24;
const SWEEP_DEADLINE_MS = 4000;

// --- Pure, unit-testable helpers -------------------------------------------

const PS_LINE = /^\s*(\d+)\s+(\d+)\s+(.+)$/;

// Strip a `unix://` or `unix:` scheme prefix to a bare filesystem path.
function stripUnixScheme(token) {
  if (token.startsWith("unix://")) {
    return token.slice("unix://".length); // unix:///abs -> /abs, unix://p -> p
  }
  if (token.startsWith("unix:")) {
    return token.slice("unix:".length); // unix:/abs -> /abs
  }
  return token;
}

// Extract the socket path a `codex app-server` (via `--listen unix://…`) or a
// `codex app-server proxy` (via `--sock /path`) line binds/targets, or null.
// Handles: `--listen unix:///abs`, `--listen unix:/abs`, a bare `unix://path`
// token, `--sock /abs`, and the `--flag=value` equals forms.
export function parseAppServerSocket(args) {
  if (typeof args !== "string") {
    return null;
  }
  const tokens = args.split(/\s+/).filter(Boolean);

  // A `unix:` scheme token covers `--listen unix://…` and bare `unix://…`.
  for (const tok of tokens) {
    if (tok.startsWith("unix:")) {
      const p = stripUnixScheme(tok);
      if (p) {
        return p;
      }
    }
    if (tok.startsWith("--listen=")) {
      const p = stripUnixScheme(tok.slice("--listen=".length));
      if (p) {
        return p;
      }
    }
    if (tok.startsWith("--sock=")) {
      const p = tok.slice("--sock=".length);
      if (p) {
        return p;
      }
    }
  }

  // `--sock <path>` (proxy form) carries a bare filesystem path, no scheme.
  const sockIdx = tokens.indexOf("--sock");
  if (sockIdx >= 0 && sockIdx + 1 < tokens.length) {
    return tokens[sockIdx + 1] || null;
  }

  return null;
}

// Socket paths that a LIVE `codex app-server proxy` currently targets. A proxy
// dies by pipe-EOF the instant its owning daemon/session exits, so a live proxy
// on a socket proves the server behind it is in use, not orphaned.
export function liveProxySockets(psOutput) {
  const sockets = new Set();
  if (typeof psOutput !== "string" || psOutput.length === 0) {
    return sockets;
  }
  for (const line of psOutput.split("\n")) {
    const match = PS_LINE.exec(line);
    if (!match) {
      continue;
    }
    const args = match[3];
    if (!args.includes("app-server proxy")) {
      continue;
    }
    const sock = parseAppServerSocket(args);
    if (sock) {
      sockets.add(sock);
    }
  }
  return sockets;
}

// Parse `ps -Ao pid,ppid,args` output and return pids of ORPHANED codex
// app-server processes. A candidate is ppid==1 AND argv has both `bin/codex`
// (excludes the desktop app at Resources/codex) and `app-server`. A codex
// app-server does NOT self-daemonize: `codex app-server --listen unix://…`
// keeps its `node …/bin/codex app-server` launcher parent alive with the native
// listener as the launcher's child, so a healthy in-use server's ppid is its
// launcher and is never 1. A server reaches ppid==1 ONLY once its launcher has
// died, so ppid==1 already means orphaned. As a DEFENSIVE extra layer we still
// spare the rare ppid==1 server that a live `app-server proxy` consumer is
// referencing on the same socket (e.g. a daemon mid-retry whose launcher briefly
// exited), and never treat a proxy line itself as a server. If a candidate's
// socket cannot be parsed we still reap it: ppid==1 + bin/codex + app-server is
// itself the orphan signal.
export function codexOrphansToReap(psOutput) {
  if (typeof psOutput !== "string" || psOutput.length === 0) {
    return [];
  }
  const proxied = liveProxySockets(psOutput);
  const pids = [];
  const seen = new Set();
  for (const line of psOutput.split("\n")) {
    const match = PS_LINE.exec(line);
    if (!match) {
      continue; // header row or blank line
    }
    const pid = Number.parseInt(match[1], 10);
    const ppid = Number.parseInt(match[2], 10);
    const args = match[3];
    if (ppid !== 1) {
      continue;
    }
    if (!args.includes("bin/codex") || !args.includes("app-server")) {
      continue;
    }
    if (args.includes("app-server proxy")) {
      continue; // a proxy is a consumer, never an orphaned server
    }
    const sock = parseAppServerSocket(args);
    if (sock && proxied.has(sock)) {
      continue; // defensive: a live proxy still consumes this server -> spare it
    }
    if (!seen.has(pid)) {
      seen.add(pid);
      pids.push(pid);
    }
  }
  return pids;
}

// True when a temp-dir name looks like a broker session dir (prefix + suffix).
export function isStaleCandidateName(name) {
  if (typeof name !== "string") {
    return false;
  }
  return BROKER_DIR_PREFIXES.some(
    (prefix) => name.startsWith(prefix) && name.length > prefix.length,
  );
}

// --- Liveness probes --------------------------------------------------------

function isPidAlive(pid) {
  if (!Number.isInteger(pid) || pid <= 0) {
    return false;
  }
  try {
    process.kill(pid, 0);
    return true;
  } catch (err) {
    // EPERM: process exists but we may not signal it -> treat as alive (protect).
    // ESRCH: no such process -> dead.
    return err.code === "EPERM";
  }
}

function readBrokerPid(pidPath) {
  try {
    const raw = fs.readFileSync(pidPath, "utf8").trim();
    const pid = Number.parseInt(raw, 10);
    return Number.isInteger(pid) && pid > 0 ? pid : null;
  } catch {
    return null;
  }
}

// Resolve to true only if a listener accepts the connection within the timeout.
// Any error/refusal/timeout resolves false (not-live-via-socket).
function probeSocketLive(sockPath, timeoutMs = SOCKET_PROBE_MS) {
  return new Promise((resolve) => {
    let settled = false;
    const socket = net.createConnection({ path: sockPath });
    const finish = (live) => {
      if (settled) {
        return;
      }
      settled = true;
      clearTimeout(timer);
      try {
        socket.destroy();
      } catch {
        // ignore
      }
      resolve(live);
    };
    const timer = setTimeout(() => finish(false), timeoutMs);
    if (typeof timer.unref === "function") {
      timer.unref();
    }
    socket.on("connect", () => finish(true));
    socket.on("error", () => finish(false));
  });
}

// --- Side-effecting steps ---------------------------------------------------

function readProcessTable() {
  try {
    return execFileSync("ps", ["-Ao", "pid,ppid,args"], {
      encoding: "utf8",
      maxBuffer: 16 * 1024 * 1024,
    });
  } catch {
    return "";
  }
}

function reapOrphans() {
  const pids = codexOrphansToReap(readProcessTable());
  let reaped = 0;
  for (const pid of pids) {
    try {
      process.kill(pid, "SIGTERM");
      reaped += 1;
    } catch {
      // ESRCH (already gone) or EPERM (not ours): nothing to reap here.
    }
  }
  return reaped;
}

// Decide whether a candidate dir is provably stale, and remove it if so.
// A dir is removed only when a broker.pid file names a process that is gone
// AND no live listener answers on broker.sock. Missing pid file, live pid, or
// a live socket all mean "leave it alone".
async function removeIfStale(dir) {
  const pidPath = path.join(dir, PID_FILE);
  const sockPath = path.join(dir, SOCKET_FILE);

  const pidExists = fs.existsSync(pidPath);
  if (!pidExists) {
    return false; // no dead-pid evidence -> cannot prove stale
  }
  if (isPidAlive(readBrokerPid(pidPath))) {
    return false; // owning broker still running
  }

  // pid file present and its process is gone. Double-check the socket has no
  // listener before deleting, so a surprising live listener is still spared.
  if (fs.existsSync(sockPath) && (await probeSocketLive(sockPath))) {
    return false;
  }

  try {
    fs.rmSync(dir, { recursive: true, force: true });
    return true;
  } catch {
    return false;
  }
}

async function sweepStaleDirs() {
  const tmp = os.tmpdir();
  let entries;
  try {
    entries = fs.readdirSync(tmp);
  } catch {
    return 0;
  }

  const candidates = [];
  for (const name of entries) {
    if (!isStaleCandidateName(name)) {
      continue;
    }
    const full = path.join(tmp, name);
    try {
      if (fs.statSync(full).isDirectory()) {
        candidates.push(full);
      }
    } catch {
      // ignore entries that vanish or cannot be stat'd
    }
  }

  const deadline = Date.now() + SWEEP_DEADLINE_MS;
  let removed = 0;
  let index = 0;

  async function worker() {
    while (index < candidates.length && Date.now() < deadline) {
      const dir = candidates[index++];
      if (await removeIfStale(dir)) {
        removed += 1;
      }
    }
  }

  const pool = Array.from(
    { length: Math.min(SWEEP_CONCURRENCY, candidates.length) },
    () => worker(),
  );
  await Promise.all(pool);
  return removed;
}

async function main() {
  let reaped = 0;
  let removed = 0;
  try {
    reaped = reapOrphans();
  } catch {
    // never let reaping fail the session
  }
  try {
    removed = await sweepStaleDirs();
  } catch {
    // never let the sweep fail the session
  }
  console.log(
    `reaped ${reaped} orphan broker(s), removed ${removed} stale dir(s)`,
  );
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  main()
    .catch(() => {})
    .finally(() => process.exit(0));
}
