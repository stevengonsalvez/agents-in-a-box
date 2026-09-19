// The inbox page, mounted: what a person actually sees for a frame, and what
// the one control does (D3p-c).
//
// The projection is held by `src/inbox.test.ts`; this mounts the component the
// window mounts, so a page that draws none of it, or a button that sends
// nothing, fails here rather than passing on a projection nobody renders.

import { settle } from "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createComponent, createSignal } from "solid-js";
import { render } from "solid-js/web";
import type { InboxRowFrame_Serialize, InboxView_Serialize } from "../../../../ainb-app/bindings/AppState";
import { Inbox } from "../../src/inbox.tsx";
import type { RendererIntent } from "../../src/tabs.ts";

const READ_AT = 1_700_000_500_000;

function row(n: number, read: number | null = null): InboxRowFrame_Serialize {
  return {
    id: `01J0INBOX0000000000000000${n}`,
    kind: "issue",
    event: "issue_created",
    subject_id: `issue-${n}`,
    summary: `issue ${n} opened`,
    recipient: "operator",
    created_at: READ_AT - n * 60_000,
    read_at: read,
  };
}

function frame(over: Partial<InboxView_Serialize> = {}): InboxView_Serialize {
  return {
    entries: [row(1), row(2, READ_AT)],
    unread: 1,
    recipient: "operator",
    absent: null,
    unreachable: null,
    rows_cut: 0,
    summaries_cut: 0,
    received_at_ms: READ_AT,
    ...over,
  };
}

let cleanup: (() => void) | undefined;
afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  document.body.innerHTML = "";
});

/** Mount the page the way `main.tsx` does: a fresh frame per read. */
async function open(first: InboxView_Serialize = frame()) {
  const [held, setHeld] = createSignal(first);
  const sent: RendererIntent[] = [];
  const container = document.createElement("div");
  document.body.appendChild(container);
  cleanup = render(
    () =>
      createComponent(Inbox, {
        get inbox() {
          return held();
        },
        onChoose: (intent: RendererIntent) => sent.push(intent),
        onClose: () => sent.push({ Command: ["closed", null] }),
      }),
    container,
  );
  await settle();
  return { sent, setHeld, container };
}

const text = () => document.body.textContent ?? "";
const sweep = () => document.querySelector<HTMLButtonElement>("button.inbox-sweep");

test("the page draws a row per entry, unread marked, with the age the frame dates it", async () => {
  await open();
  const rows = [...document.querySelectorAll("li.inbox-row")];
  assert.deepEqual(
    rows.map((entry) => [entry.getAttribute("data-entry"), entry.classList.contains("unread")]),
    [
      [row(1).id, true],
      [row(2).id, false],
    ],
  );
  assert.match(rows[0].textContent ?? "", /issue · issue_created/);
  assert.match(rows[0].textContent ?? "", /issue 1 opened/);
  assert.match(rows[0].textContent ?? "", /1m/, "the age, not a locale timestamp");
  assert.doesNotMatch(text(), /\d{4}/, "no year, no clock time");
});

test("the sweep is offered while something is unread, sends the command, and goes once the daemon says nothing is unread", async () => {
  const { sent, setHeld } = await open();
  assert.equal(sweep()?.disabled, false);

  sweep()?.click();
  await settle();
  assert.deepEqual(sent, [{ Command: ["inbox.mark_all_read", null] }], "one command, nothing else");

  // The daemon's reply is what clears it: the reducer folds the sweep and the
  // next frame carries unread 0, so the button goes with the count.
  setHeld(frame({ unread: 0, entries: [row(1, READ_AT), row(2, READ_AT)] }));
  await settle();
  assert.equal(sweep()?.disabled, true, "nothing unread: nothing to sweep");
  assert.equal([...document.querySelectorAll("li.inbox-row.unread")].length, 0);
});

test("an absent inbox says why and offers no sweep; an unreachable one keeps its rows", async () => {
  const { setHeld } = await open(
    frame({ entries: [], unread: 0, absent: "the daemon serves no hangar/inbox_list" }),
  );
  assert.match(text(), /the daemon serves no hangar\/inbox_list/);
  assert.equal(sweep()?.disabled, true);
  assert.equal(document.querySelector("section.inbox")?.getAttribute("data-state"), "absent");

  setHeld(frame({ unreachable: "daemon not reachable" }));
  await settle();
  assert.match(text(), /daemon not reachable/);
  assert.equal([...document.querySelectorAll("li.inbox-row")].length, 2, "the last rows stay");
  assert.equal(document.querySelector("section.inbox")?.getAttribute("data-state"), "unreachable");
});

test("an empty inbox says so, and both cut counters draw", async () => {
  const { setHeld } = await open(frame({ entries: [], unread: 0 }));
  assert.match(text(), /Nothing in the inbox/);

  setHeld(frame({ rows_cut: 4, summaries_cut: 1 }));
  await settle();
  assert.match(text(), /4 older entries not sent, 1 summary shortened/);
  assert.doesNotMatch(text(), /Nothing in the inbox/);
});

test("a row keeps its node across a frame that only moves the count", async () => {
  const { setHeld } = await open();
  const first = document.querySelector("li.inbox-row");
  setHeld(frame({ unread: 1 }));
  await settle();
  assert.equal(document.querySelector("li.inbox-row"), first, "the row was patched, not rebuilt");
});
