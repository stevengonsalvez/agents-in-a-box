// The webview half of parity: a fixture's committed FRAMES rendered by the
// real components, checked against the same expected-facts list the ratatui
// half is checked against (`ainb-app/tests/parity.rs`).
//
// A webview never sees an AppState, it sees frames, so this reads
// `ainb-app/tests/parity/frames/<fixture>.json` (written by
// `ainb-app/tests/parity_frames.rs`) rather than building any state of its
// own. One fixture, two renderers, one list of facts.
//
// The list has to be able to fail, and it has to fail for the right reason:
// the second test takes a file out of the FRAME this half is given and proves
// the facts that file carried are reported missing. Editing the HTML after it
// was rendered would only prove the comparator works.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import solid from "vite-plugin-solid";
import type { GitViewView_Serialize, PluginsHostView_Serialize, UsageView } from "../../../ainb-app/bindings/AppState";

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

/**
 * Server-render the review tab over `fixture`'s committed git view frame,
 * with `change` applied to that frame first: what a renderer given less would
 * have drawn.
 */
async function drawReview(
  fixture: string,
  change: (gitView: GitViewView_Serialize) => void = () => {},
): Promise<string> {
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
    change(gitView);
    return renderToString(() => Review({ gitView, stale: false, onChoose() {} }));
  } finally {
    await server.close();
  }
}

/**
 * Server-render the Commits tab over `fixture`'s committed git view frame,
 * with `change` applied to that frame first.
 */
async function drawCommits(
  fixture: string,
  change: (gitView: GitViewView_Serialize) => void = () => {},
): Promise<string> {
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
    const { Commits } = await server.ssrLoadModule("/src/commits.tsx");
    const { renderToString } = await server.ssrLoadModule("solid-js/web");
    const gitView = framed(fixture, "git_view") as GitViewView_Serialize;
    change(gitView);
    return renderToString(() => Commits({ gitView, stale: false, onChoose() {} }));
  } finally {
    await server.close();
  }
}

test("the commits tab shows every fact the fixture's list names", async () => {
  const html = await drawCommits("git_commits");

  assert.ok(facts("git_commits").length > 0, "the facts list has facts in it");
  assert.deepEqual(missing(html, "git_commits"), [], text(html));
});

// Two hundred commits with the hundredth selected: neither renderer draws the
// whole list, and the facts are the commits around that selection, so a window
// that drew from row zero, or a list pinned to the top of the branch, fails
// them.
test("the commits tab draws the page the selection is in", async () => {
  const html = await drawCommits("git_commits_long");

  assert.ok(facts("git_commits_long").length > 0, "the facts list has facts in it");
  assert.deepEqual(missing(html, "git_commits_long"), [], text(html));
  assert.ok(
    !html.includes("commit number 0<"),
    "the first commit of the branch is not drawn: this is a window, not the whole list",
  );
});

test("a renderer given one commit fewer fails the facts", async () => {
  const whole = await drawCommits("git_commits");
  assert.deepEqual(missing(whole, "git_commits"), [], "the fixture as it stands shows every fact");

  const lost = await drawCommits("git_commits", (gitView) => {
    gitView.git_view_state?.commits.shift();
  });

  assert.notDeepEqual(
    missing(lost, "git_commits"),
    [],
    "a render missing a whole commit still showed every expected fact, so the list proves nothing",
  );
});

// The review tab draws a window of its body now, not the whole of it (#1221),
// so a fixture bigger than one window would only ever show the facts inside
// that window. `git_review` is three small files and fits in one, which is why
// the list below can still name every fact the tab draws.
test("the review tab shows every fact the fixture's list names", async () => {
  const html = await drawReview("git_review");

  assert.ok(facts("git_review").length > 0, "the facts list has facts in it");
  assert.deepEqual(missing(html, "git_review"), [], text(html));
});

test("a renderer given one file fewer fails the facts", async () => {
  const whole = await drawReview("git_review");
  assert.deepEqual(missing(whole, "git_review"), [], "the fixture as it stands shows every fact");

  const lost = await drawReview("git_review", (gitView) => {
    gitView.git_view_state?.review.files.shift();
  });

  assert.notDeepEqual(
    missing(lost, "git_review"),
    [],
    "a render missing a whole file still showed every expected fact, so the list proves nothing",
  );
});

/**
 * Server-render the plugin placeholder for `screen` over `fixture`'s committed
 * plugins_host frame (D3p-f), with `change` applied to that frame first.
 */
async function drawPlaceholder(
  fixture: string,
  screen: string,
  change: (pluginsHost: PluginsHostView_Serialize) => void = () => {},
): Promise<string> {
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
    const { PluginPlaceholder } = await server.ssrLoadModule("/src/plugin_placeholder.tsx");
    const { renderToString } = await server.ssrLoadModule("solid-js/web");
    const pluginsHost = framed(fixture, "plugins_host") as PluginsHostView_Serialize;
    change(pluginsHost);
    return renderToString(() => PluginPlaceholder({ screen, pluginsHost }));
  } finally {
    await server.close();
  }
}

test("the plugin placeholder shows every fact the plugins_host list names", async () => {
  const html = await drawPlaceholder("plugins_host", "witr");

  assert.ok(facts("plugins_host").length > 0, "the facts list has facts in it");
  assert.deepEqual(missing(html, "plugins_host"), [], text(html));
});

test("a placeholder given no render error fails the plugins_host facts", async () => {
  const lost = await drawPlaceholder("plugins_host", "witr", (pluginsHost) => {
    delete pluginsHost.plugin_render_errors.witr;
  });

  assert.notDeepEqual(
    missing(lost, "plugins_host"),
    [],
    "a render without the recorded error still showed every expected fact, so the list proves nothing",
  );
});

for (const [fixture, screen] of [
  ["plugin_not_registered", "learnings"],
  ["plugin_connecting", "abtop"],
] as const) {
  test(`the plugin placeholder shows every fact the ${fixture} list names`, async () => {
    const html = await drawPlaceholder(fixture, screen);

    assert.ok(facts(fixture).length > 0, "the facts list has facts in it");
    assert.deepEqual(missing(html, fixture), [], text(html));
  });
}

/**
 * Server-render the stats tab over `fixture`'s committed usage frame, with
 * `change` applied to that frame first.
 *
 * `stats` is a DOM-half fixture only: the terminal's stats is burndown's
 * plugin paint on `analytics`, and a second built-in beside it would be the
 * drift the section set stops (spec, D3-prime parity amendment).
 */
async function drawStats(fixture: string, change: (usage: UsageView) => void = () => {}): Promise<string> {
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
    const { Stats } = await server.ssrLoadModule("/src/stats.tsx");
    const { renderToString } = await server.ssrLoadModule("solid-js/web");
    const usage = framed(fixture, "usage") as UsageView;
    change(usage);
    return renderToString(() => Stats({ usage }));
  } finally {
    await server.close();
  }
}

test("the stats tab shows every fact the stats fixture's list names", async () => {
  const html = await drawStats("stats");

  assert.ok(facts("stats").length > 0, "the facts list has facts in it");
  assert.deepEqual(missing(html, "stats"), [], text(html));
});

test("a stats tab given one model fewer fails the facts", async () => {
  const lost = await drawStats("stats", (usage) => {
    usage.summary?.models.shift();
  });

  assert.notDeepEqual(
    missing(lost, "stats"),
    [],
    "a render missing a whole model still showed every expected fact, so the list proves nothing",
  );
});
