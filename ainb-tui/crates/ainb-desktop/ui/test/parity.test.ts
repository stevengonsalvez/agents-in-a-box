// The DOM half of parity: the settings page itself, `SettingsPage`, rendered
// from the frames a window receives for a committed fixture, checked against
// the same expected-facts list the ratatui half checks its snapshot against
// (`ainb-core/tests/parity_snapshots.rs`). One fixture, two renderers, one
// list. The frames are `ainb-app/tests/parity/frames/<fixture>.json`, dumped
// by `ainb-app/tests/parity_frames.rs`; the facts are
// `facts/<fixture>.txt` beside the fixtures, the one list both halves read.
//
// The component is mounted for real through Solid's server renderer (see
// `solid-ssr.mjs`), so a fact the page stops printing fails here. The suite
// has to be able to fail, against the drawing and not a string edited after
// the fact: the mutation test takes a daemon row out of what the PAGE is
// given, renders again, and asserts the facts that row carried are missing.

import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { renderToString } from "solid-js/web";
import type { ConfigView_Serialize, HangarView_Serialize } from "../../../ainb-app/bindings/AppState";
import { editRefusal, SECRET_REASON } from "../src/settings.ts";
import { SettingsPage } from "../src/settings.tsx";

const PARITY_DIR = join(dirname(fileURLToPath(import.meta.url)), "../../../ainb-app/tests/parity");

/** The fixtures the settings page draws: each has a facts list beside it. */
const SETTINGS_FIXTURES = ["config", "daemons"] as const;

/** The facts of `fixture`, comments and blank lines dropped. */
function facts(fixture: string): string[] {
  return readFileSync(join(PARITY_DIR, "facts", `${fixture}.txt`), "utf8")
    .split("\n")
    .map((line) => line.trimEnd())
    .filter((line) => line !== "" && !line.startsWith("#"));
}

/** The framed sections of `fixture`, as the window would hold them. */
function frames(fixture: string): { config: ConfigView_Serialize; hangar: HangarView_Serialize } {
  return JSON.parse(readFileSync(join(PARITY_DIR, "frames", `${fixture}.json`), "utf8"));
}

/**
 * The page's text, one line per element, as a person reads it. `change`
 * edits the frames before the page is given them: what a renderer that lost
 * something would have drawn.
 */
export function pageText(
  fixture: string,
  change: (frames: { config: ConfigView_Serialize; hangar: HangarView_Serialize }) => void = () => undefined,
): string[] {
  const held = frames(fixture);
  change(held);
  const { config, hangar } = held;
  const html = renderToString(() =>
    SettingsPage({
      config,
      revision: 1,
      hangar,
      sidecar: { state: "starting" },
      setup: null,
      run: () => undefined,
      onSetupWrite: () => undefined,
      onRefreshSetup: () => undefined,
      onClose: () => undefined,
    }),
  );
  return html
    .replace(/<[^>]+>/g, "\n")
    .split("\n")
    .map((line) => line.replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&amp;/g, "&").replace(/&quot;/g, '"').replace(/&#39;/g, "'").trim())
    .filter((line) => line !== "");
}

/** The facts no line of `lines` contains. */
export function missingFacts(lines: readonly string[], expected: readonly string[]): string[] {
  return expected.filter((fact) => !lines.some((line) => line.includes(fact)));
}

for (const fixture of SETTINGS_FIXTURES) {
  test(`the settings page rendered from the ${fixture} fixture's frames carries every expected fact`, () => {
    const expected = facts(fixture);
    assert.ok(expected.length > 0, `facts/${fixture}.txt lists facts`);
    const lines = pageText(fixture);
    assert.ok(lines.length > 10, "the page rendered");
    assert.deepEqual(missingFacts(lines, expected), [], `facts missing from the settings page for ${fixture}`);
  });
}

test("every fixture the settings page draws has a facts list", () => {
  const lists = readdirSync(join(PARITY_DIR, "facts"));
  for (const fixture of SETTINGS_FIXTURES) {
    assert.ok(lists.includes(`${fixture}.txt`), `facts/${fixture}.txt`);
    assert.ok(readdirSync(PARITY_DIR).includes(`${fixture}.json`), `${fixture}.json`);
  }
});

test("a page that loses a daemon row fails the facts", () => {
  const expected = facts("daemons");
  assert.deepEqual(missingFacts(pageText("daemons"), expected), [], "the fixture as it stands shows every fact");

  const lost = pageText("daemons", ({ hangar }) => {
    hangar.daemons_state.shared!.rows.shift();
  });
  assert.ok(
    missingFacts(lost, expected).length > 0,
    "a page missing a daemon row still showed every expected fact, so the list proves nothing",
  );
});

test("the page's edit policy is the reducer's, row for row", () => {
  // `ainb-app/tests/config_renderer_edits.rs` writes one verdict per registry
  // row; the page's copy (#1224) must give the same verdict for every one.
  const verdicts = readFileSync(join(PARITY_DIR, "../fixtures/renderer_editable_rows.txt"), "utf8")
    .split("\n")
    .filter((line) => line !== "")
    .map((line) => line.split(" ") as [string, string]);
  assert.ok(verdicts.length > 100, "the fixture lists the registry");
  const wrong = verdicts.filter(([verdict, key]) => {
    const refusal = editRefusal(key, verdict === "secret");
    return verdict === "allow" ? refusal !== null : verdict === "secret" ? refusal !== SECRET_REASON : refusal === null;
  });
  assert.deepEqual(wrong, [], "rows where the page and the reducer disagree");
});
