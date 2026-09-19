// The private world the journey runs in: its own HOME, its own hangar home and
// its own tmux server, seeded with real sessions by the real `ainb` CLI.
//
//   <world>/home   HOME, and AINB_HANGAR_HOME under it (never the box's own)
//   <world>/tmux   TMUX_TMPDIR, so every tmux server here is this run's
//   <world>/bin    first on PATH: the `claude` the seeded sessions run
//   <world>/repo   the git repository the sessions are worktrees of
//
// Nothing is killed by pattern: tmux sessions go by exact name, and the daemon
// is stopped through its own verb.

import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readdirSync, rmSync, statSync, writeFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { OWNER_FILE } from "./cleanup.js";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));

/**
 * The desktop binary under test: a debug build with the `wdio` feature on,
 * which is what `cargo build --features wdio` in `crates/ainb-desktop` writes.
 */
export const APP_BIN = process.env.AINB_DESKTOP_BIN ?? resolve(HERE, "../target/debug/ainb-desktop");

/** The CLI the world seeds with, and that the journey creates a session from. */
export const AINB_BIN = process.env.AINB_BIN ?? resolve(HERE, "../../../target/debug/ainb");

/**
 * The daemon the shell starts as its sidecar. A bundle carries it beside the
 * app; a debug build takes this path from the environment, so the journey
 * hands it the one the desktop workspace built.
 */
export const DAEMON_BIN =
  process.env.AINB_DESKTOP_DAEMON_BIN ?? resolve(HERE, "../../../target/debug/ainb-hangar-daemon");

/**
 * The agent a seeded session runs: it prints a tick a second, echoes back
 * anything typed at it, and reads a file on request. That makes the pane's own
 * capture the proof that a line typed in the window reached the process, and
 * gives the recorded read an end the harness can see.
 */
const FIXTURE_AGENT = `#!/usr/bin/env bash
trap 'echo "AGENT GOT SIGINT"' INT
n=0
while :; do
  n=$((n + 1))
  echo "agent tick $n"
  if IFS= read -r -t 1 line; then
    case "$line" in
      bulk\\ *) cat "\${line#bulk }"; echo "BULK DONE" ;;
      *) echo "agent read: $line" ;;
    esac
  fi
done
`;

/**
 * The world, as the environment carries it. The launcher builds it in
 * `onPrepare` and puts it here, so every worker the launcher forks after that,
 * and the app the service launches from a worker, sees the same one.
 */
const WORLD_ENV = "AINB_E2E_WORLD";

function world() {
  const raw = process.env[WORLD_ENV];
  if (raw === undefined) throw new Error("the world is not up");
  return JSON.parse(raw);
}

/** The world's environment, once it is up. */
export function env() {
  world();
  return process.env;
}

/** Run a command in the world, returning its stdout. */
export function run(command, args, options = {}) {
  return execFileSync(command, args, {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    timeout: 180_000,
    ...options,
    env: { ...env(), ...(options.env ?? {}) },
  });
}

/** What a seeded session's own tmux pane shows. */
export function paneText(tmux) {
  try {
    return run("tmux", ["capture-pane", "-t", `=${tmux}:`, "-p"]);
  } catch {
    return "";
  }
}

/**
 * Fail the run when the webview the app embeds is older than the webview's
 * source.
 *
 * `cargo build` bakes whatever is in `ui/dist` into the binary and never
 * rebuilds it, so a checkout that moved on without `npm run build` launches a
 * window from an older bundle: every spec then tests a webview nobody wrote,
 * and the failures read as regressions in the app. Run `npm run build` in
 * `ui/` to clear this.
 */
function freshBundle() {
  const dist = resolve(HERE, "../ui/dist/index.html");
  if (!existsSync(dist)) throw new Error(`the webview is not built: ${dist}`);
  const built = statSync(dist).mtimeMs;
  const src = resolve(HERE, "../ui/src");
  // The sources, and the files outside them that change what a build
  // produces: the page the bundle is injected into, the manifest, the
  // lockfile a dependency bump moves, and the build's own config.
  const inputs = readdirSync(src, { recursive: true })
    .map((entry) => join(src, entry))
    .concat(
      ["index.html", "package.json", "package-lock.json", "vite.config.ts"].map((name) =>
        resolve(HERE, "../ui", name),
      ),
    )
    .filter((path) => existsSync(path));
  const newest = inputs.map((path) => statSync(path).mtimeMs).reduce((a, b) => Math.max(a, b), 0);
  if (newest > built) {
    throw new Error(
      `the webview bundle is older than ui/src (${new Date(built).toISOString()} vs ${new Date(newest).toISOString()}): run npm run build in ui/`,
    );
  }
}

/**
 * Create the world and seed `sessions` of them. Called from `onPrepare`, so
 * every wdio worker, and the app the service launches, inherits this
 * environment: nothing outside the world is read or written.
 */
export function up(sessions = 2) {
  for (const [name, path] of [
    ["the desktop app", APP_BIN],
    ["the ainb CLI", AINB_BIN],
    ["the hangar daemon", DAEMON_BIN],
  ]) {
    if (!existsSync(path)) throw new Error(`${name} is not built: ${path}`);
  }
  freshBundle();

  const root = mkdtempSync(join(tmpdir(), "ainb-e2e-"));
  // The run that owns this world, so a later run's cleanup leaves it alone
  // while this process lives (cleanup.js).
  writeFileSync(join(root, OWNER_FILE), String(process.pid));
  const home = join(root, "home");
  const hangar = join(home, ".agents-in-a-box");
  const bin = join(root, "bin");
  const repo = join(root, "repo");
  for (const dir of [join(hangar, "config"), join(root, "tmux"), bin, repo]) mkdirSync(dir, { recursive: true });
  writeFileSync(join(bin, "claude"), FIXTURE_AGENT, { mode: 0o755 });

  delete process.env.TMUX;
  delete process.env.TMUX_PANE;
  Object.assign(process.env, {
    HOME: home,
    AINB_HANGAR_HOME: hangar,
    TMUX_TMPDIR: join(root, "tmux"),
    // A debug build takes its sidecar from here; a release build never does.
    AINB_DESKTOP_DAEMON_BIN: DAEMON_BIN,
    // The daemon lists live sessions through this CLI to find where an answer
    // goes. Unset, it runs its own binary, which has no `list`, finds no
    // session and delivers nothing.
    AINB_BIN,
    PATH: `${bin}:${dirname(AINB_BIN)}:${process.env.PATH ?? ""}`,
    [WORLD_ENV]: JSON.stringify({ root, seeded: [] }),
  });

  // `ainb init` is not run: the desktop reads no onboarding record, and the
  // init path reaches the network, which a journey must not wait on. The two
  // files it would write that matter here are written directly.
  writeFileSync(join(hangar, "install.json"), '{"agents":[],"hook_script":"","prompt_dismissed":true}\n');

  run("git", ["init", "-q", "-b", "main", repo]);
  run("git", ["-C", repo, "-c", "user.email=e2e@localhost", "-c", "user.name=e2e", "commit", "-q", "--allow-empty", "-m", "init"]);
  const state = world();
  for (let i = 0; i < sessions; i += 1) state.seeded.push(seed());
  // The workers the launcher forks after this inherit the finished world.
  process.env[WORLD_ENV] = JSON.stringify(state);
  return state;
}

/** One more session from the CLI, as a separate process, and its ids. */
export function seed() {
  const repo = join(world().root, "repo");
  // No --format json: the command prints the labelled lines `field` parses
  // whatever is asked for.
  const out = run(AINB_BIN, ["run", "--repo", repo, "--worktree"], { cwd: repo });
  const field = (label) => out.split("\n").find((line) => line.trim().startsWith(label))?.split(":").slice(1).join(":").trim();
  const session = {
    id: field("Session ID"),
    tmux: field("Tmux Session"),
    cwd: field("Working Dir"),
    branch: field("Branch"),
  };
  if (!session.tmux) throw new Error(`ainb run created no session:\n${out}`);
  return session;
}

/**
 * Append one hook event to the daemon's ingest file, from a separate process,
 * exactly as an agent's hook does: the daemon tails this file and raises what
 * the line announces.
 *
 * Written directly rather than through `ainb fleet atc hook`, because that
 * command refuses a line with no session id, and the sidebar only takes a
 * question raised with none (#1049). The line carries no `raw_payload_ref`:
 * the hook sets one only beside a payload file it wrote, and a ref with no
 * file, or one that is not the line's own event id, holds the daemon's ingest
 * on a retry for good. With none, the daemon reads the inline payload.
 */
export function hook(line) {
  const file = join(env().AINB_HANGAR_HOME, "events.jsonl");
  run("sh", ["-c", 'printf \'%s\\n\' "$1" >> "$2"', "hook", JSON.stringify(line), file]);
}

/** The sessions seeded before the app launched. */
export function seeded() {
  return world().seeded;
}

/** Stop everything this world started and remove it. */
export function down() {
  if (process.env[WORLD_ENV] === undefined) return;
  try {
    run(AINB_BIN, ["hangar", "daemon", "stop"], { stdio: "ignore" });
  } catch {
    // Already gone, or never started: the world is removed either way.
  }
  let names = [];
  try {
    names = run("tmux", ["list-sessions", "-F", "#{session_name}"]).split("\n").filter(Boolean);
  } catch {
    // No server left on this world's socket directory.
  }
  for (const name of names) {
    try {
      run("tmux", ["kill-session", "-t", `=${name}`]);
    } catch {
      // A session that ended between the listing and here.
    }
  }
  // A daemon still writing can hold a directory open for a moment.
  try {
    rmSync(world().root, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  } catch (error) {
    console.warn(`the world at ${world().root} was left behind: ${error.message}`);
  }
  delete process.env[WORLD_ENV];
}
