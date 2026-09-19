// The banner names the binary to move and the verb; the daemons panel names
// the daemon's version and range.

import assert from "node:assert/strict";
import { test } from "node:test";
import { banner, retryable, sidecarDaemonLine, STOP_DAEMON, type SidecarState } from "./sidecar.ts";

const MESSAGE = "daemon protocol 5-6 cannot serve a client speaking 1-1; restart from the newer binary";

test("a newer daemon: the app is the one to update, and the daemon's sentence is kept verbatim", () => {
  const text = banner({ state: "incompatible", message: MESSAGE, daemon_is_newer: true });
  assert.ok(text.startsWith(MESSAGE), text);
  assert.match(text, /update the app/);
  assert.doesNotMatch(text, /stop/i);
});

test("an older daemon: the banner names the stop verb the operator runs", () => {
  const text = banner({ state: "incompatible", message: MESSAGE, daemon_is_newer: false });
  assert.ok(text.startsWith(MESSAGE), text);
  assert.ok(text.includes(STOP_DAEMON), text);
  assert.equal(STOP_DAEMON, "ainb daemon hangar-daemon stop");
  assert.doesNotMatch(text, /update the app/);
});

test("retry is offered where the operator moved something, not while connecting", () => {
  const cases: [SidecarState, boolean][] = [
    [{ state: "starting" }, false],
    [{ state: "connected", spawned: true, daemon_version: "1.28.2", protocol: { min: 1, max: 1 } }, false],
    [{ state: "reconnecting", error: "dropped" }, false],
    [{ state: "incompatible", message: MESSAGE, daemon_is_newer: true }, true],
    [{ state: "degraded", error: "nothing answered", has_log: true }, true],
  ];
  for (const [state, expected] of cases) assert.equal(retryable(state), expected, state.state);
});

test("the daemons panel names the version and the range, and says when it cannot", () => {
  assert.equal(
    sidecarDaemonLine({ state: "connected", spawned: false, daemon_version: "1.28.2", protocol: { min: 1, max: 1 } }),
    "This window is on hangar daemon 1.28.2, protocol 1.",
  );
  assert.equal(
    sidecarDaemonLine({ state: "connected", spawned: true, daemon_version: null, protocol: { min: 1, max: 2 } }),
    "This window is on hangar daemon a version it does not report, protocol 1-2, started by this app.",
  );
  assert.equal(sidecarDaemonLine({ state: "starting" }), null);
  assert.equal(sidecarDaemonLine({ state: "incompatible", message: MESSAGE, daemon_is_newer: true }), null);
});
