// The webview half of parity: a fixture's committed FRAMES rendered by the
// real components, checked against the same expected-facts list the ratatui
// half is checked against (`ainb-app/tests/parity.rs`).
//
// A webview never sees an AppState, it sees frames, so this reads
// `ainb-app/tests/parity/frames/<fixture>.json` (written by
// `ainb-app/tests/parity_frames.rs`) rather than building any state of its
// own. One fixture, two renderers, one list of facts.
//
// The list has to be able to fail: `a fact missing from the render is caught`
// takes one fact out of what this half drew and proves the check reports it,
// because a suite that cannot fail says nothing about a stubbed renderer.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import solid from "vite-plugin-solid";
import type { GitViewView_Serialize } from "../../../ainb-app/bindings/AppState";

const parityDir = new URL("../../../ainb-app/tests/parity/", import.meta.url);

/** The facts `fixture` must show, comments and blank lines dropped. */
function facts(fixture: string): string[] {
  return readFileSync(new URL(`facts/${fixture}.txt`, parityDir), "utf8")
    .split("\n")
    .map((line) => line.trimEnd())
    .filter((line) => line.length > 0 && !line.startsWith("#"));
}

/** The `section` body of `fixture`'s committed frames. */
function framed(fixture: string, section: string): unknown {
  const frames = JSON.parse(readFileSync(new URL(`frames/${fixture}.json`, parityDir), "utf8"));
  return frames[section];
}

/**
 * `html` as the text a person reads: tags dropped, entities decoded, runs of
 * space closed up.
 *
 * A fact is what the screen says, not the markup it says it in; the ratatui
 * half joins its own lines for the same reason.
 */
function text(html: string): string {
  return html
    .replace(/<[^>]*>/g, " ")
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&amp;/g, "&")
    .replace(/[ \t]+/g, " ");
}

/** Every fact of `fixture` that `html` does not show. */
function missing(html: string, fixture: string): string[] {
  const drawn = text(html);
  return facts(fixture).filter((fact) => !drawn.includes(fact));
}

/** Server-render the review tab over `fixture`'s committed git view frame. */
async function drawReview(fixture: string): Promise<string> {
  const server = await createServer({
    configFile: false,
    root: fileURLToPath(new URL("..", import.meta.url)),
    plugins: [solid({ ssr: true })],
    server: { middlewareMode: true, hmr: false },
    appType: "custom",
    ssr: { noExternal: ["solid-js"] },
    logLevel: "silent",
  });
  try {
    const { Review } = await server.ssrLoadModule("/src/review.tsx");
    const { renderToString } = await server.ssrLoadModule("solid-js/web");
    const gitView = framed(fixture, "git_view") as GitViewView_Serialize;
    return renderToString(() => Review({ gitView, stale: false, onChoose() {} }));
  } finally {
    await server.close();
  }
}

test("the review tab shows every fact the fixture's list names", async () => {
  const html = await drawReview("git_review");

  assert.ok(facts("git_review").length > 0, "the facts list has facts in it");
  assert.deepEqual(missing(html, "git_review"), [], text(html));
});

test("a fact missing from the render is caught", async () => {
  const html = await drawReview("git_review");
  const dropped = facts("git_review")[0];

  const mutated = html.replaceAll(dropped, "");

  assert.deepEqual(missing(mutated, "git_review"), [dropped]);
  assert.deepEqual(missing(html, "git_review"), []);
});
