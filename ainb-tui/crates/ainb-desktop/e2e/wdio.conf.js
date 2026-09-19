// The desktop journey's runner. The app under test is a debug build with the
// `wdio` feature on, which carries the embedded WebDriver the service drives;
// a release build carries no driver at all, which the bundle smoke asserts.
//
// The world is built in `onPrepare` and torn down in `onComplete`, so the
// workers the launcher forks after it, and the app the service launches, all
// inherit its HOME, hangar home and tmux server.
//
// One world per launcher, and so one runner at a time (#1160). The embedded
// provider spawns the app once, from the launcher's own `onPrepare`, before any
// worker exists: a world per worker could not reach it. The spec files run one
// after another against that one app and that one world, in the order listed,
// and a config that asks for two at once is refused rather than left to share
// a HOME, a hangar home and a tmux server whose teardown pulls the ground from
// under the other.

import { APP_BIN, down, up } from "./world.js";
import { cleanUpBeforeRun } from "./cleanup.js";

export const config = {
  runner: "local",
  specs: ["./specs/journey.e2e.js", "./specs/answer.e2e.js"],
  maxInstances: 1,
  framework: "mocha",
  reporters: ["spec"],
  logLevel: "warn",
  // One journey, driving a real daemon and real tmux sessions end to end:
  // every leg waits on the product, and creating a session is real work.
  mochaOpts: { ui: "bdd", timeout: 600_000 },

  capabilities: [
    {
      browserName: "tauri",
      "tauri:options": { application: APP_BIN },
    },
  ],
  // `embedded` needs no external driver: the app serves WebDriver itself on
  // the port the service passes it.
  services: [["@wdio/tauri-service", { driverProvider: "embedded", captureBackendLogs: true }]],

  onPrepare(config, capabilities) {
    refuseConcurrentRuns(config, capabilities);
    // What earlier runs left: their worlds and this worktree's daemons.
    cleanUpBeforeRun();
    up(2);
  },
  onComplete() {
    down();
  },
};

/** Refuse any config that would run two workers, or two windows, on one world. */
export function refuseConcurrentRuns(config, capabilities) {
  const reasons = [];
  if (!Array.isArray(capabilities)) reasons.push("a multiremote run drives two windows");
  else if (capabilities.length !== 1) reasons.push(`${capabilities.length} capabilities`);
  // The tightest bound wins, so any one of them at 1 keeps the run serial.
  const workers = Math.min(
    config.maxInstances ?? Infinity,
    config.maxInstancesPerCapability ?? Infinity,
    ...(Array.isArray(capabilities) ? capabilities : []).map((cap) => cap["wdio:maxInstances"] ?? Infinity),
  );
  if (workers !== 1) reasons.push(`up to ${workers} workers at once`);
  if (reasons.length > 0) {
    throw new Error(`the journey runs one runner on one world (#1160), refused: ${reasons.join(", ")}`);
  }
}
