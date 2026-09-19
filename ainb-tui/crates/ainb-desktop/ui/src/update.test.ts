// The updater's framed states, one line each.

import assert from "node:assert/strict";
import { test } from "node:test";
import { terminal, updateLine } from "./update.ts";

test("each phase reads as one line, and a download names its progress", () => {
  assert.equal(updateLine(null), null);
  assert.equal(updateLine({ phase: "checking" }), "Checking for updates...");
  assert.equal(
    updateLine({ phase: "downloading", received: 12 * 1024 * 1024, total: 40 * 1024 * 1024 }),
    "Downloading update: 12 MB of 40 MB (30%)",
  );
  assert.equal(
    updateLine({ phase: "downloading", received: 512 * 1024, total: null }),
    "Downloading update: 0.5 MB",
  );
  assert.equal(updateLine({ phase: "verifying" }), "Verifying the download...");
  assert.equal(updateLine({ phase: "applying" }), "Installing the update...");
  assert.equal(updateLine({ phase: "installed", version: "1.29.0" }), "Update 1.29.0 installed: restarting");
  assert.equal(updateLine({ phase: "failed", reason: "checksum mismatch" }), "Update failed: checksum mismatch");
});

test("a terminal phase is what clears the line, a working one is not", () => {
  assert.equal(terminal({ phase: "downloading", received: 1, total: null }), false);
  assert.equal(terminal({ phase: "installed", version: "1.29.0" }), true);
  assert.equal(terminal({ phase: "failed", reason: "x" }), true);
});
