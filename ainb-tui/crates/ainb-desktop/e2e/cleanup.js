// Suite-start cleanup: what earlier runs left behind, and only that.
//
// A run that crashed or was interrupted can leave its world under the temp
// directory and its daemon running. The next run clears both before it builds
// its own, without touching anything another run on this box still owns.
// Nothing is killed by name: a daemon is matched by the exact path of the
// executable it runs from, and killed by its pid.

import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));

/** The file a world records its owning process in. */
export const OWNER_FILE = "owner.pid";

/** A world with no owner file is only removed once it is at least this old. */
const UNOWNED_GRACE_MS = 60 * 60 * 1000;

/** Whether a process with `pid` is running. */
function alive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error.code === "EPERM";
  }
}

/**
 * Remove `ainb-e2e-*` worlds earlier runs left under `dir`: a world whose
 * owning process has exited, or, for a world that recorded no owner, one older
 * than an hour. A world whose owner still runs belongs to a run in progress,
 * possibly another lane's on the same box, and is left alone. Returns the
 * worlds removed.
 */
export function removeStaleWorlds(dir = tmpdir(), now = Date.now()) {
  const removed = [];
  for (const name of readdirSync(dir)) {
    if (!name.startsWith("ainb-e2e-")) continue;
    const world = join(dir, name);
    let stale;
    try {
      const owner = Number.parseInt(readFileSync(join(world, OWNER_FILE), "utf8"), 10);
      stale = !Number.isInteger(owner) || !alive(owner);
    } catch {
      try {
        stale = now - statSync(world).mtimeMs > UNOWNED_GRACE_MS;
      } catch {
        continue;
      }
    }
    if (stale) {
      rmSync(world, { recursive: true, force: true });
      removed.push(world);
    }
  }
  return removed;
}

/**
 * The target directories of THIS worktree whose daemons the suite may stop:
 * the workspace's own and the desktop workspace's.
 */
export function worktreeTargetDirs() {
  return [resolve(HERE, "../../../target"), resolve(HERE, "../target")];
}

/**
 * Stop every `ainb-hangar-daemon` running from one of `targets`, by its pid.
 *
 * A process is matched on the executable path it was started from (the first
 * argument `ps` reports), which must sit under one of `targets` and be named
 * `ainb-hangar-daemon`: a daemon another worktree, an installed ainb or
 * another lane started is never touched. Returns the pids signalled.
 */
export function stopWorktreeDaemons(targets = worktreeTargetDirs()) {
  let listing;
  try {
    listing = execFileSync("ps", ["-Ao", "pid=,args="], { encoding: "utf8" });
  } catch {
    return [];
  }
  const roots = targets.map((target) => resolve(target) + "/");
  const stopped = [];
  for (const line of listing.split("\n")) {
    const match = line.trim().match(/^(\d+)\s+(\S+)/);
    if (!match) continue;
    const [, pid, executable] = match;
    if (Number(pid) === process.pid) continue;
    if (basename(executable) !== "ainb-hangar-daemon") continue;
    if (!roots.some((root) => resolve(executable).startsWith(root))) continue;
    try {
      process.kill(Number(pid), "SIGTERM");
      stopped.push(Number(pid));
    } catch {
      // Gone already.
    }
  }
  return stopped;
}

/** Everything suite start clears, logged so a run says what it removed. */
export function cleanUpBeforeRun() {
  const worlds = removeStaleWorlds();
  const daemons = stopWorktreeDaemons();
  if (worlds.length > 0) console.log(`e2e: removed stale worlds ${worlds.join(", ")}`);
  if (daemons.length > 0) console.log(`e2e: stopped this worktree's daemons ${daemons.join(", ")}`);
}
