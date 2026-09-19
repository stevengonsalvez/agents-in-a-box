// The ACP card's transcript keeps a chunk's DOM node across frames that do
// not change it (#1267).
//
// `transcriptView` builds new chunk objects on every read, so before the fix
// every line of a transcript was re-created whenever the Fleet frame moved:
// a selection a person was making over the text went with them.

import { settle } from "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createComponent, createSignal } from "solid-js";
import { render } from "solid-js/web";
import type { FleetView_Serialize } from "../../../ainb-app/bindings/AppState";
import { transcriptView } from "../../src/acp.ts";
import { AcpCard } from "../../src/acp.tsx";

const SESSION = "claude:pa";

/** A Fleet frame carrying a transcript, new objects on every call. */
function fleet(lastBody: string): FleetView_Serialize {
  return {
    transcript: {
      session_key: SESSION,
      status: "running",
      starts_part_way: false,
      chunks: [
        { order: 1, kind: "message", body: "first", truncated: false },
        { order: 2, kind: "thinking", body: "second", truncated: false },
        { order: 3, kind: "message", body: lastBody, truncated: false },
      ],
    },
  } as unknown as FleetView_Serialize;
}

let cleanup: (() => void) | undefined;
afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  document.body.innerHTML = "";
});

/** The card, drawing the transcript the way `main.tsx` passes it: per read. */
async function open() {
  const [frame, setFrame] = createSignal(fleet("third"));
  const container = document.createElement("div");
  document.body.appendChild(container);
  cleanup = render(
    () =>
      createComponent(AcpCard, {
        sessionKey: SESSION,
        get view() {
          return transcriptView(frame(), SESSION);
        },
        onClose: () => undefined,
      }),
    container,
  );
  await settle();
  return {
    setFrame,
    container,
    chunks: () => [...container.querySelectorAll<HTMLElement>(".acp-chunk")],
  };
}

test("a frame that changes nothing keeps every chunk's node", async () => {
  const card = await open();
  const chunks = card.chunks();
  assert.equal(chunks.length, 3);

  card.setFrame(fleet("third"));
  await settle();

  // Compared with ===, not assert.equal: a failing assert.equal inspects both
  // nodes, and a happy-dom node's object graph takes minutes to print.
  card.chunks().forEach((node, index) => assert.ok(node === chunks[index], `chunk ${index} was rebuilt`));
});

test("the chunks a new frame does not change keep their nodes across 50 frames", async () => {
  const card = await open();
  const first = card.chunks()[0];
  assert.ok(first);

  // Each frame rewrites the LAST chunk, as a running agent does.
  for (let n = 0; n < 50; n += 1) {
    card.setFrame(fleet(`third ${n}`));
    await settle();
  }

  assert.ok(first.isConnected, "the first chunk is no longer on screen");
  assert.ok(card.chunks()[0] === first, "the first chunk was rebuilt");
  assert.equal(first.querySelector(".acp-body")?.textContent, "first");
});

test("a chunk whose body grows keeps its node and shows the new text", async () => {
  const card = await open();
  const last = card.chunks().at(-1);
  assert.ok(last);

  card.setFrame(fleet("third, continued"));
  await settle();

  assert.ok(card.chunks().at(-1) === last, "the growing chunk was rebuilt");
  assert.equal(last.querySelector(".acp-body")?.textContent, "third, continued");
});
