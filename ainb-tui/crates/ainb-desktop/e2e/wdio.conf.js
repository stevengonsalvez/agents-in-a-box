// The desktop journey's runner. The app under test is a debug build with the
// `wdio` feature on, which carries the embedded WebDriver the service drives;
// a release build carries no driver at all, which the bundle smoke asserts.
//
// The world is built in `onPrepare` and torn down in `onComplete`, so the
// workers the launcher forks after it, and the app the service launches from a
// worker, all inherit its HOME, hangar home and tmux server.

import { APP_BIN, down, up } from "./world.js";

export const config = {
  runner: "local",
  specs: ["./specs/**/*.e2e.js"],
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

  onPrepare() {
    up(2);
  },
  onComplete() {
    down();
  },
};
