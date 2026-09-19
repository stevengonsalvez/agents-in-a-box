// The commit rows keep their DOM nodes across frames that do not change them
// (#1267), the same rule the board and the palette follow.
//
// Mounted for real into a happy-dom document (see `window.ts`): the nodes the
// test holds are the nodes a pointer would be over. A list rebuilt on every
// frame is not a cosmetic fault, it is a click landing on a node that has just
// been replaced, which is what the review journey hit on the palette.

import { settle } from "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createComponent, createSignal } from "solid-js";
import { render } from "solid-js/web";
import type { GitViewView_Serialize } from "../../../ainb-app/bindings/AppState";
import { Commits } from "../../src/commits.tsx";
import type { RendererIntent } from "../../src/tabs.ts";

/** A git view frame carrying three commits, new objects on every call. */
function frame(selected = 0, messages = ["first", "second", "third"]): GitViewView_Serialize {
  return {
    git_view_state: {
      active_tab: "Commits",
      commits: messages.map((message, n) => ({
        hash_short: `c${n}`,
        author: "Sample Dev",
        date: "2026-09-19",
        message,
      })),
      commits_cut: 0,
      selected_commit_index: selected,
      selected_commit_cut: false,
      review: { files: [], files_cut: 0 },
    },
  } as unknown as GitViewView_Serialize;
}

let cleanup: (() => void) | undefined;
afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  document.body.innerHTML = "";
});

/** The tab, open over one frame, with every chosen intent recorded. */
async function open() {
  const [view, setView] = createSignal(frame());
  const chosen: RendererIntent[] = [];
  const container = document.createElement("div");
  document.body.appendChild(container);
  cleanup = render(
    () =>
      createComponent(Commits, {
        get gitView() {
          return view();
        },
        stale: false,
        onChoose: (intent: RendererIntent) => chosen.push(intent),
      }),
    container,
  );
  await settle();
  return {
    setView,
    chosen,
    rows: () => [...container.querySelectorAll<HTMLButtonElement>(".commit-row")],
    row: (sha: string) => container.querySelector<HTMLButtonElement>(`.commit-row[data-sha="${sha}"]`),
  };
}

test("a frame that changes nothing keeps every commit row", async () => {
  const tab = await open();
  const before = tab.rows();
  assert.deepEqual(
    before.map((node) => node.dataset.sha),
    ["c0", "c1", "c2"],
  );

  tab.setView(frame());
  await settle();

  const after = tab.rows();
  assert.equal(after.length, before.length);
  for (const [at, node] of after.entries()) {
    assert.equal(node, before[at], `row ${node.dataset.sha} was replaced by a frame that changed nothing`);
  }
});

test("a frame that moves the selection keeps the rows it did not change", async () => {
  const tab = await open();
  const first = tab.row("c0");
  const third = tab.row("c2");

  tab.setView(frame(2));
  await settle();

  assert.equal(tab.row("c0"), first, "an untouched row keeps its node");
  assert.equal(tab.row("c2"), third, "and so does the one the selection moved to");
  assert.equal(tab.row("c2")?.getAttribute("aria-current"), "true", "which now says it is current");
});

test("a frame that changes a message redraws that row in place", async () => {
  const tab = await open();
  const second = tab.row("c1");

  tab.setView(frame(0, ["first", "second, amended", "third"]));
  await settle();

  assert.equal(tab.row("c1"), second, "the row is patched, not replaced");
  assert.match(second?.textContent ?? "", /second, amended/);
});
