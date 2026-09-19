// The palette's rows keep their DOM nodes across frames that do not change
// them (#1267). The host sends a new sessions frame several times a second; a
// row rebuilt on each one is a row a click can land on after it was removed,
// which is how the D3 journey lost its palette click for 60 s.
//
// Mounted for real into a happy-dom document (see `window.ts`), so the test
// holds the very nodes a person's pointer would.

import { hostReplies, settle } from "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createComponent, createSignal } from "solid-js";
import { render } from "solid-js/web";
import type { Session_Serialize, SessionsView_Serialize } from "../../../ainb-app/bindings/AppState";
import type { PaletteEntry } from "../../bindings/Desktop.ts";
import { Palette } from "../../src/palette.tsx";
import { openRowIntent, type RendererIntent } from "../../src/tabs.ts";

const ENTRIES: PaletteEntry[] = [
  { id: "session_list.git", doc: "Show git view", context: "session_list", chord: "g", active: true },
  { id: "session_list.refresh", doc: "Refresh workspaces", context: "session_list", chord: "f", active: true },
];

function session(id: string, name = id): Session_Serialize {
  return { id, name, status: "Running", branch_name: `agents/${id}`, workspace_path: "/repo" } as unknown as Session_Serialize;
}

/** A sessions frame as the host sends it: a new object every time. */
function frame(sessions: Session_Serialize[]): SessionsView_Serialize {
  return { workspaces: [{ name: "repo", sessions }], selected_session_id: null } as unknown as SessionsView_Serialize;
}

let cleanup: (() => void) | undefined;
afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  document.body.innerHTML = "";
});

/** The palette, open over `sessions`, with every chosen intent recorded. */
async function open(sessions: Session_Serialize[]) {
  hostReplies.set("palette", ENTRIES);
  const [view, setView] = createSignal(frame(sessions));
  const chosen: RendererIntent[] = [];
  const container = document.createElement("div");
  document.body.appendChild(container);
  cleanup = render(
    () =>
      createComponent(Palette, {
        get sessions() {
          return view();
        },
        onChoose: (intent: RendererIntent) => chosen.push(intent),
        onClose: () => undefined,
      }),
    container,
  );
  await settle();
  const rows = () => [...container.querySelectorAll<HTMLButtonElement>(".palette-row")];
  const row = (key: string) => container.querySelector<HTMLButtonElement>(`.palette-row[data-row="${key}"]`);
  return { setView, chosen, rows, row, container };
}

test("a frame with the same rows keeps every row's node", async () => {
  const sessions = [session("a"), session("b")];
  const palette = await open(sessions);
  const before = palette.rows();
  assert.deepEqual(
    before.map((node) => node.dataset.row),
    ["session:a", "session:b", "command:session_list.git", "command:session_list.refresh"],
  );

  // The same rows, in new objects, as the next frame carries them.
  palette.setView(frame([session("a"), session("b")]));
  await settle();

  const after = palette.rows();
  assert.equal(after.length, before.length);
  // Compared with ===, not assert.equal: a failing assert.equal inspects both
  // nodes, and a happy-dom node's object graph takes minutes to print.
  after.forEach((node, index) => assert.ok(node === before[index], `row ${node.dataset.row} was rebuilt`));
});

test("a click on a row after 50 frames still runs that row", async () => {
  const palette = await open([session("a"), session("b")]);
  // Held the way a pointer holds it: found once, clicked later.
  const sessionRow = palette.row("session:b");
  const commandRow = palette.row("command:session_list.git");
  assert.ok(sessionRow && commandRow);

  for (let n = 0; n < 50; n += 1) {
    palette.setView(frame([session("a"), session("b")]));
    await settle();
  }

  assert.ok(sessionRow.isConnected, "the session row is still the one on screen");
  assert.ok(commandRow.isConnected, "the command row is still the one on screen");
  commandRow.click();
  assert.deepEqual(palette.chosen, [{ Command: ["session_list.git", null] }]);
});

test("a session row clicked after 50 frames opens that session", async () => {
  const palette = await open([session("a"), session("b")]);
  const sessionRow = palette.row("session:b");
  assert.ok(sessionRow);

  for (let n = 0; n < 50; n += 1) {
    palette.setView(frame([session("a"), session("b")]));
    await settle();
  }

  sessionRow.click();
  assert.deepEqual(palette.chosen, [openRowIntent({ session: "b" })]);
});

test("a row whose label changes keeps its node and shows the new label", async () => {
  const palette = await open([session("a"), session("b")]);
  const node = palette.row("session:b");
  assert.ok(node);

  palette.setView(frame([session("a"), session("b", "renamed")]));
  await settle();

  assert.ok(palette.row("session:b") === node, "the renamed row is the same node");
  assert.equal(node.querySelector(".palette-title")?.textContent, "renamed");
});

test("moving the cursor touches only the row it leaves and the row it reaches", async () => {
  const palette = await open([session("a"), session("b"), session("c")]);
  const input = palette.container.querySelector<HTMLInputElement>(".palette-query");
  assert.ok(input);

  const touched = new Set<string>();
  const observer = new MutationObserver((records) => {
    for (const record of records) {
      const row = (record.target as Element).closest?.(".palette-row") as HTMLElement | null;
      touched.add(row?.dataset.row ?? `(${(record.target as Element).nodeName})`);
    }
  });
  observer.observe(palette.container, { subtree: true, attributes: true, childList: true, characterData: true });

  input.dispatchEvent(new window.KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
  await settle();
  observer.disconnect();

  assert.deepEqual([...touched].sort(), ["session:a", "session:b"]);
  assert.equal(palette.row("session:b")?.getAttribute("aria-selected"), "true");
});
