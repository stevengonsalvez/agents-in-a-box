// The answer banner keeps its DOM nodes across frames that carry no question
// for a moment, and goes once the grace after the last such frame runs out
// (#1266). Mounted for real into a happy-dom document (see `window.ts`), so
// the test holds the very option nodes a person's pointer would.

import { settle } from "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createComponent, createSignal } from "solid-js";
import { render } from "solid-js/web";
import type { AskState_Serialize, AttentionMark_Serialize, SessionsView_Serialize } from "../../../ainb-app/bindings/AppState";
import { AnswerSlot } from "../../src/answer.tsx";
import { type Question, questionFor } from "../../src/answer.ts";
import type { RendererIntent } from "../../src/tabs.ts";

// The banner paints through the window's animation frame; happy-dom has one,
// Node does not.
const dom = window as unknown as { requestAnimationFrame: typeof requestAnimationFrame; cancelAnimationFrame: typeof cancelAnimationFrame };
Object.defineProperty(globalThis, "requestAnimationFrame", { value: dom.requestAnimationFrame.bind(window), configurable: true, writable: true });
Object.defineProperty(globalThis, "cancelAnimationFrame", { value: dom.cancelAnimationFrame.bind(window), configurable: true, writable: true });

function mark(over: Partial<AttentionMark_Serialize> = {}): AttentionMark_Serialize {
  return {
    kind: "Ask",
    detail: "Which environment?",
    request: "att-7",
    options: [
      { label: "staging", description: "" },
      { label: "production", description: "" },
      { label: "local", description: "" },
    ],
    route: "Daemon",
    ...over,
  };
}

/** A sessions frame with the selected row blocking on `attention`: a new object every time. */
function frame(...attention: AttentionMark_Serialize[]): SessionsView_Serialize {
  return {
    workspaces: [{ name: "repo", sessions: [{ id: "u-1", name: "api", attention }] }],
    selected_workspace_index: 0,
    selected_session_id: "u-1",
    shell_selected: false,
  } as unknown as SessionsView_Serialize;
}

const ASK: AskState_Serialize = { request: "att-7", focus: "Options", cursor: 0, free_text_len: 0, phases: [] } as AskState_Serialize;

let cleanup: (() => void) | undefined;
afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  document.body.innerHTML = "";
});

/** The slot mounted over a question signal, with a clock the test moves and a short grace. */
async function mount(grace: number) {
  let clock = 1_000;
  const [question, setQuestion] = createSignal<Question | null>(questionFor(frame(mark())));
  const sent: RendererIntent[][] = [];
  const container = document.createElement("div");
  document.body.appendChild(container);
  cleanup = render(
    () =>
      createComponent(AnswerSlot, {
        get question() {
          return question();
        },
        ask: ASK,
        run: async (intents: RendererIntent[]) => {
          sent.push(intents);
        },
        now: () => clock,
        grace,
      }),
    container,
  );
  await settle();
  const options = () => [...container.querySelectorAll<HTMLButtonElement>(".answer-banner .answer-option")];
  const banner = () => container.querySelector(".answer-banner");
  return { setQuestion, sent, options, banner, tick: (ms: number) => (clock += ms) };
}

test("a bare frame inside the grace keeps every option node, and a click still picks", async () => {
  const slot = await mount(5_000);
  const before = slot.options();
  assert.equal(before.length, 3);

  // The frame carries no question for a moment: the banner stays, node for node.
  slot.tick(1_000);
  slot.setQuestion(null);
  await settle();
  const during = slot.options();
  assert.equal(during.length, 3, "the banner is still up");
  for (let i = 0; i < 3; i += 1) assert.ok(during[i] === before[i], `option ${i} is the same node`);

  // The next frame carries it again: still the same nodes.
  slot.tick(1_000);
  slot.setQuestion(questionFor(frame(mark())));
  await settle();
  const after = slot.options();
  for (let i = 0; i < 3; i += 1) assert.ok(after[i] === before[i], `option ${i} survived the round trip`);

  after[1].click();
  await settle();
  assert.equal(slot.sent.length, 1, "the click sent its intents");
  const last = slot.sent[0].at(-1) as { Command: [string, { request: string; index: number; label: string }] };
  assert.equal(last.Command[0], "session_list.ask.pick");
  assert.deepEqual(last.Command[1], { request: "att-7", index: 1, label: "production" });
});

test("the banner goes once the grace after the last frame that carried it runs out", async () => {
  const slot = await mount(40);
  assert.ok(slot.banner(), "up while the frame carries the question");

  slot.tick(10);
  slot.setQuestion(null);
  await settle();
  assert.ok(slot.banner(), "a bare frame inside the grace keeps it");

  // No frame comes; the slot's own timer ends the grace.
  slot.tick(100);
  await new Promise((resolve) => setTimeout(resolve, 80));
  await settle();
  assert.equal(slot.banner(), null, "released by the timer, not by a frame");

  // A later frame with a new question mounts a fresh banner.
  slot.setQuestion(questionFor(frame(mark({ request: "att-8" }))));
  await settle();
  assert.equal(slot.banner()?.getAttribute("data-request"), "att-8");
});

test("a new request mounts a fresh banner in place of the old one", async () => {
  const slot = await mount(5_000);
  const first = slot.banner();
  slot.tick(1_000);
  slot.setQuestion(questionFor(frame(mark({ request: "att-8" }))));
  await settle();
  // The drawn question follows one paint behind; give the frame its paint.
  await new Promise((resolve) => setTimeout(resolve, 30));
  await settle();
  const second = slot.banner();
  assert.ok(second !== null && second !== first, "a new element for the new request");
  assert.equal(second.getAttribute("data-request"), "att-8");
});
