// The DOM half of parity: the settings page rendered from the frames a window
// receives for a committed fixture, checked against the same expected-facts
// list the ratatui half checks its snapshot against
// (`ainb-core/tests/parity_snapshots.rs`). One fixture, two renderers, one
// list. The frames are `ainb-app/tests/parity/frames/<fixture>.json`, dumped by
// `ainb-app/tests/parity_frames.rs`; the facts are `<fixture>.facts` beside the
// fixture.
//
// The suite has to be able to fail: the last test deletes one fact from the
// rendered output and asserts the check reports it.

import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import type { ConfigView_Serialize, HangarView_Serialize } from "../../../ainb-app/bindings/AppState";
import { settingsLines } from "./settings.ts";

const PARITY_DIR = join(dirname(fileURLToPath(import.meta.url)), "../../../ainb-app/tests/parity");

/** The fixtures the settings page draws: each has a facts list beside it. */
const SETTINGS_FIXTURES = ["config", "daemons"] as const;

/** The facts of `fixture`, comments and blank lines dropped. */
function facts(fixture: string): string[] {
  return readFileSync(join(PARITY_DIR, `${fixture}.facts`), "utf8")
    .split("\n")
    .map((line) => line.trimEnd())
    .filter((line) => line !== "" && !line.startsWith("#"));
}

/** The framed sections of `fixture`, as the window would hold them. */
function frames(fixture: string): { config: ConfigView_Serialize; hangar: HangarView_Serialize } {
  return JSON.parse(readFileSync(join(PARITY_DIR, "frames", `${fixture}.json`), "utf8"));
}

/** The facts no line of `lines` contains. */
export function missingFacts(lines: readonly string[], expected: readonly string[]): string[] {
  return expected.filter((fact) => !lines.some((line) => line.includes(fact)));
}

for (const fixture of SETTINGS_FIXTURES) {
  test(`the settings page drawn from the ${fixture} fixture's frames carries every expected fact`, () => {
    const expected = facts(fixture);
    assert.ok(expected.length > 0, `${fixture}.facts lists facts`);
    const { config, hangar } = frames(fixture);
    const lines = settingsLines(config, hangar);
    assert.deepEqual(missingFacts(lines, expected), [], `facts missing from the settings page for ${fixture}`);
  });
}

test("every fixture the settings page draws has a facts list, and no facts list names a fixture that is gone", () => {
  const entries = readdirSync(PARITY_DIR);
  for (const fixture of SETTINGS_FIXTURES) {
    assert.ok(entries.includes(`${fixture}.facts`), `${fixture}.facts`);
    assert.ok(entries.includes(`${fixture}.json`), `${fixture}.json`);
  }
  for (const entry of entries.filter((name) => name.endsWith(".facts"))) {
    const fixture = entry.slice(0, -".facts".length);
    assert.ok(entries.includes(`${fixture}.json`), `${entry} names a fixture that does not exist`);
  }
});

test("the check fails when one fact is deleted from the rendered output", () => {
  const expected = facts("config");
  const { config, hangar } = frames("config");
  const lines = settingsLines(config, hangar);
  assert.deepEqual(missingFacts(lines, expected), []);

  const deleted = expected[expected.length - 1]!;
  const mutated = lines.map((line) => line.split(deleted).join(""));
  assert.deepEqual(missingFacts(mutated, expected), [deleted]);
});
