// The settings page's three lists (the category tree, the rows of the
// selected node, and the daemons table) keep their DOM nodes across frames
// that do not change them (#1267).
//
// The frames are the committed parity fixtures, so the page draws what a real
// window draws, and each "frame" is a fresh clone of them, as the host sends.

import { settle } from "./window.ts";

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { afterEach, test } from "node:test";
import { fileURLToPath } from "node:url";
import { createComponent, createSignal } from "solid-js";
import { render } from "solid-js/web";
import type { ConfigView_Serialize, HangarView_Serialize } from "../../../ainb-app/bindings/AppState";
import { SettingsPage } from "../../src/settings.tsx";
import type { RendererIntent } from "../../src/tabs.ts";

const PARITY = join(dirname(fileURLToPath(import.meta.url)), "../../../../ainb-app/tests/parity/frames");

function fixture(name: string): { config: ConfigView_Serialize; hangar: HangarView_Serialize } {
  return JSON.parse(readFileSync(join(PARITY, `${name}.json`), "utf8"));
}

const CONFIG = fixture("config").config;
const DAEMONS = fixture("daemons").hangar;

/** One frame's worth of sections, cloned: new objects, the same content. */
function frames() {
  return {
    config: structuredClone(CONFIG),
    hangar: structuredClone(DAEMONS),
  };
}

let cleanup: (() => void) | undefined;
afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  document.body.innerHTML = "";
});

/** The settings page over the fixtures, with every intent it sends recorded. */
async function open() {
  const [view, setView] = createSignal(frames());
  const sent: RendererIntent[] = [];
  const container = document.createElement("div");
  document.body.appendChild(container);
  cleanup = render(
    () =>
      createComponent(SettingsPage, {
        get config() {
          return view().config;
        },
        get hangar() {
          return view().hangar;
        },
        revision: 1,
        sidecar: { kind: "connected", daemon: "local" } as never,
        setup: null,
        run: (intents: RendererIntent[]) => sent.push(...intents),
        onSetupWrite: () => undefined,
        onRefreshSetup: () => undefined,
        onClose: () => undefined,
      }),
    container,
  );
  await settle();
  return {
    setView,
    sent,
    container,
    nodes: () => [...container.querySelectorAll<HTMLElement>(".settings-node")],
    node: (id: string) => container.querySelector<HTMLElement>(`.settings-node[data-node="${id}"]`),
    rows: () => [...container.querySelectorAll<HTMLElement>(".settings-row")],
    daemons: () => [...container.querySelectorAll<HTMLElement>("tr[data-daemon]")],
  };
}

test("a frame that changes nothing keeps every node, row and daemon line", async () => {
  const page = await open();
  const nodes = page.nodes();
  const rows = page.rows();
  const daemons = page.daemons();
  assert.ok(nodes.length > 0, "the tree drew no node");
  assert.ok(rows.length > 0, "the page drew no setting row");
  assert.ok(daemons.length > 0, "the daemons table drew no line");

  page.setView(frames());
  await settle();

  // Compared with ===, not assert.equal: a failing assert.equal inspects both
  // nodes, and a happy-dom node's object graph takes minutes to print.
  page.nodes().forEach((node, index) => assert.ok(node === nodes[index], `node ${node.dataset.node} was rebuilt`));
  page.rows().forEach((row, index) => assert.ok(row === rows[index], `row ${row.dataset.key} was rebuilt`));
  page
    .daemons()
    .forEach((row, index) => assert.ok(row === daemons[index], `daemon ${row.dataset.daemon} was rebuilt`));
});

test("a tree node clicked after 50 frames still selects that node", async () => {
  const page = await open();
  const first = page.nodes()[0];
  const id = first?.dataset.node;
  assert.ok(first && id);
  // Held the way a pointer holds it: the button inside the node, found once.
  const button = [...first.querySelectorAll<HTMLButtonElement>("button")].at(-1);
  assert.ok(button);

  for (let n = 0; n < 50; n += 1) {
    page.setView(frames());
    await settle();
  }

  assert.ok(button.isConnected, "the node's button is still the one on screen");
  button.click();
  assert.deepEqual(page.sent.at(-1), { Command: ["config.select_node", { id }] });
});

test("a setting row keeps its node and its widget across 50 frames", async () => {
  const page = await open();
  const row = page.rows()[0];
  assert.ok(row);
  const widget = row.querySelector("input, select");
  assert.ok(widget, "the row drew no widget");

  for (let n = 0; n < 50; n += 1) {
    page.setView(frames());
    await settle();
  }

  assert.ok(page.rows()[0] === row, "the row was rebuilt");
  assert.ok(row.querySelector("input, select") === widget, "the row's widget was rebuilt");
});

test("a tree click names the node's own id, not the key the list drew it under", async () => {
  const page = await open();
  // Every node, not just the first: the id in the intent must be the one the
  // projection gave the node, whatever key the list drew it under.
  for (const node of page.nodes()) {
    const id = node.dataset.node;
    const button = [...node.querySelectorAll<HTMLButtonElement>("button")].at(-1);
    assert.ok(button && id);
    button.click();
    assert.deepEqual(
      page.sent.at(-1),
      { Command: ["config.select_node", { id }] },
      `the click under key for ${id} named another node`,
    );
  }

  // The chevron sends the same id through toggleNode.
  const parent = page.nodes().find((node) => node.querySelector(".chevron"));
  if (parent) {
    const chevron = parent.querySelector<HTMLButtonElement>(".chevron");
    assert.ok(chevron);
    const before = page.sent.length;
    chevron.click();
    const sent = page.sent.slice(before);
    // toggleNode selects the node, then asks the reducer to expand it; only
    // the select carries an id, and it must be the node's own.
    assert.deepEqual(sent[0], { Command: ["config.select_node", { id: parent.dataset.node }] });
    assert.ok(sent.length > 1, "the chevron sent no expand");
  }
});

test("an edit from a row's widget after 50 frames names that row", async () => {
  const page = await open();
  // A text row: its widget is an input the page sends edits from.
  const row = page.rows().find((candidate) => {
    const input = candidate.querySelector<HTMLInputElement>("input");
    return input !== null && input.type === "text" && !candidate.classList.contains("readonly");
  });
  assert.ok(row, "no editable text row on the page");
  const key = row.dataset.key;
  const input = row.querySelector<HTMLInputElement>("input");
  assert.ok(input && key);

  for (let n = 0; n < 50; n += 1) {
    page.setView(frames());
    await settle();
  }

  assert.ok(input.isConnected, "the row's input is no longer on screen");
  input.value = "edited-by-test";
  input.dispatchEvent(new window.Event("change", { bubbles: true }));
  await settle();

  const last = page.sent.at(-1);
  assert.ok(last, "the edit sent nothing");
  assert.ok(
    JSON.stringify(last).includes("edited-by-test"),
    `the edit did not carry the typed value: ${JSON.stringify(last)}`,
  );
  assert.ok(
    JSON.stringify(last).includes(key.split("|").at(-1) ?? key),
    `the edit named another row: ${JSON.stringify(last)} for key ${key}`,
  );
});
