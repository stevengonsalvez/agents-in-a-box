// What the palette offers, how it ranks a query, and the intent a row sends.

import assert from "node:assert/strict";
import { test } from "node:test";
import type { Session_Serialize, SessionsView_Serialize } from "../../../ainb-app/bindings/AppState";
import { commandRows, PALETTE_ROWS, rank, score, sessionRows, stepRow, type PaletteEntry } from "./palette.ts";

function entry(id: string, doc: string, over: Partial<PaletteEntry> = {}): PaletteEntry {
  return { id, doc, context: "Sessions", chord: null, active: true, ...over };
}

function view(...sessions: Partial<Session_Serialize>[]): SessionsView_Serialize {
  return {
    workspaces: [{ name: "repo", sessions: sessions.map((s) => ({ status: "Running", branch_name: "main", ...s })) }],
  } as unknown as SessionsView_Serialize;
}

test("a subsequence matches and anything else does not", () => {
  assert.equal(score("", "anything"), 0);
  assert.notEqual(score("sls", "session_list.select_row"), null);
  assert.equal(score("zzz", "session_list.select_row"), null);
  // Out of order is not a match, and case does not matter.
  assert.equal(score("wor", "Row"), null);
  assert.notEqual(score("ROW", "select_row"), null);
});

test("a run and a word start score above scattered letters", () => {
  const run = score("row", "select_row")!;
  const scattered = score("row", "rename our workspace")!;
  assert.ok(run > scattered, `${run} should beat ${scattered}`);
  assert.ok(score("sl", "session_list")! > score("sl", "assemble")!);
});

test("the palette ranks the tighter match first and keeps the rest", () => {
  const rows = commandRows([
    entry("session_list.select_row", "Open the selected session"),
    entry("shell.new", "Start a shell"),
    entry("global.quit", "Quit"),
  ]);
  const ranked = rank(rows, "open");
  assert.equal(ranked[0].title, "Open the selected session");
  assert.equal(rank(rows, "").length, 3, "an empty query offers everything, in the host's order");
  assert.equal(rank(rows, "zzzz").length, 0);
});

test("an equal match prefers the row that runs now, then the one with a key", () => {
  const rows = commandRows([
    entry("a.open", "Open pane", { active: false }),
    entry("b.open", "Open pane", { chord: "ctrl+o" }),
  ]);
  const ranked = rank(rows, "open pane");
  assert.deepEqual(
    ranked.map((row) => row.key),
    ["command:b.open", "command:a.open"],
  );
  assert.equal(ranked[1].active, false, "a row the reducer would refuse is still offered");
});

test("a command row carries the command intent and a session row opens its list row", () => {
  const [command] = commandRows([entry("shell.new", "Start a shell", { chord: "ctrl+n" })]);
  assert.deepEqual(command.intent, { Command: ["shell.new", null] });
  assert.equal(command.chord, "ctrl+n");
  assert.equal(command.detail, "Sessions · shell.new");

  const [session] = sessionRows(view({ id: "u-1", name: "api", branch_name: "agents/api" }));
  assert.deepEqual(session.intent, {
    Command: ["session_list.select_row", { target: { session: "u-1" }, open: true }],
  });
  assert.equal(session.title, "api");
  assert.equal(session.detail, "running · agents/api");
});

test("a row's text is drawn as a label, so a control character cannot restyle it", () => {
  const [command] = commandRows([entry("shell.new", "Start[31m a shell")]);
  assert.equal(command.title, "Start[31m a shell");
  const [session] = sessionRows(view({ id: "u-1", name: "‮api" }));
  assert.equal(session.title, "api");
});

test("the list is capped and the selection wraps over what is left", () => {
  const many = commandRows(Array.from({ length: PALETTE_ROWS + 5 }, (_, i) => entry(`c.${i}`, `Command ${i}`)));
  assert.equal(rank(many, "command").length, PALETTE_ROWS);
  assert.equal(stepRow(3, 2, 1), 0);
  assert.equal(stepRow(3, 0, -1), 2);
  assert.equal(stepRow(0, 0, 1), 0);
});

test("a row the reducer would refuse is offered but not ranked above a live one", () => {
  const rows = commandRows([
    entry("a.go", "Go somewhere", { active: false }),
    entry("b.go", "Go somewhere", { active: true }),
  ]);
  const ranked = rank(rows, "go somewhere");
  assert.deepEqual(
    ranked.map((row) => row.active),
    [true, false],
    "the row that runs now is offered first, and the other is still offered",
  );
});
