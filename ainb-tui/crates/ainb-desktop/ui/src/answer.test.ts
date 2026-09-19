// The banner's question, the phases it reads, and the intents it sends.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { createRoot, createSignal } from "solid-js";
import type {
  AnswerPhase_Serialize,
  AskState_Serialize,
  AttentionMark_Serialize,
  SessionsView_Serialize,
} from "../../../ainb-app/bindings/AppState";
import {
  bannerKeys,
  drawnQuestion,
  latchQuestion,
  phaseOf,
  pickIntents,
  type Question,
  questionFor,
  type Refusal,
  selectedSession,
  sendInOrder,
  typedIntents,
} from "./answer.ts";

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

function sessions(...attention: AttentionMark_Serialize[]): SessionsView_Serialize {
  return {
    workspaces: [{ name: "repo", sessions: [{ id: "u-1", name: "api", attention }] }],
    selected_workspace_index: 0,
    selected_session_id: "u-1",
    shell_selected: false,
  } as unknown as SessionsView_Serialize;
}

function ask(over: Partial<AskState_Serialize> = {}, phase?: AnswerPhase_Serialize): AskState_Serialize {
  return {
    request: "att-7",
    focus: "Options",
    cursor: 0,
    free_text_len: 0,
    phases: phase === undefined ? [] : [["att-7", phase]],
    ...over,
  } as AskState_Serialize;
}

const commands = (intents: unknown[]) =>
  intents.map((intent) => {
    const it = intent as { Command?: [string, unknown]; Text?: string };
    return it.Command ? it.Command[0] : `text:${it.Text}`;
  });

test("the question is the row's own first blocking chip, with its request, options and route", () => {
  const question = questionFor(sessions(mark({ kind: "Done", request: "done-1" }), mark()))!;
  assert.equal(question.request, "att-7", "a DONE is not a question; the first blocking chip is");
  assert.equal(question.route, "daemon");
  assert.deepEqual(question.options, ["staging", "production", "local"]);
  assert.equal(question.freeText, true);
});

test("a chip merged from the daemon without its provider id still draws its options", () => {
  // The frame's mark is the reducer's chip, whatever produced it, so there is
  // no second lookup that could miss and turn a typed answer into option one.
  const question = questionFor(sessions(mark({ route: "Pane", request: "ASK:1000" })))!;
  assert.deepEqual(question.options, ["staging", "production", "local"]);
  assert.equal(question.route, "pane");
});

test("a bare approve offers exactly approve and deny, and no composer", () => {
  const question = questionFor(
    sessions(
      mark({
        kind: "Approve",
        request: "APPROVE:1",
        route: "Broker",
        options: [
          { label: "approve", description: "" },
          { label: "deny", description: "" },
        ],
      }),
    ),
  )!;
  assert.equal(question.route, "broker");
  assert.deepEqual(question.options, ["approve", "deny"]);
  assert.equal(question.freeText, false, "the broker refuses anything but the two labels");
});

test("an approve nothing can deliver offers no send at all", () => {
  const question = questionFor(sessions(mark({ kind: "Approve", request: "APPROVE:1", route: "None" })))!;
  assert.equal(question.answerable, false);
  assert.deepEqual(pickIntents(question, ask({ request: "APPROVE:1" }), 0), []);
  assert.deepEqual(typedIntents(question, ask({ request: "APPROVE:1" }), "yes"), []);
});

test("nothing blocking, nothing selected: no banner", () => {
  assert.equal(questionFor(sessions(mark({ kind: "Done" }))), null);
  const none = { ...sessions(mark()), selected_session_id: null } as unknown as SessionsView_Serialize;
  assert.equal(questionFor(none), null);
});

test("the selection is the row the frame names by id, not a position in its list", () => {
  // A frame carries only the rows its filter shows (#1180): the selected row
  // is found by id wherever it sits, and an id the frame does not carry names
  // nothing, never whatever row sits at the old position.
  const view = {
    workspaces: [
      { name: "other", sessions: [{ id: "u-0", name: "web", attention: [mark({ request: "att-0" })] }] },
      { name: "repo", sessions: [{ id: "u-1", name: "api", attention: [mark()] }] },
    ],
    selected_workspace_index: 1,
    selected_session_id: "u-1",
    shell_selected: false,
  } as unknown as SessionsView_Serialize;
  assert.equal(selectedSession(view)?.id, "u-1");
  assert.equal(questionFor(view)?.request, "att-7");
  const gone = { ...view, selected_session_id: "u-hidden" } as unknown as SessionsView_Serialize;
  assert.equal(selectedSession(gone), undefined);
  assert.equal(questionFor(gone), null);
  const shell = { ...view, shell_selected: true } as unknown as SessionsView_Serialize;
  assert.equal(selectedSession(shell), undefined);
});

test("with two open questions on one session, the banner refuses when the reducer is on the other", () => {
  // The reducer answers `ask.request`. A banner that sent against any other
  // question would move a cursor on that question's options and Enter would
  // answer it.
  const question = questionFor(sessions(mark(), mark({ request: "att-8" })))!;
  assert.equal(question.request, "att-7");
  assert.deepEqual(pickIntents(question, ask({ request: "att-8", cursor: 1 }), 1), [], "no pick");
  assert.deepEqual(typedIntents(question, ask({ request: "att-8" }), "staging"), [], "no typing");
  assert.deepEqual(pickIntents(question, undefined, 1), [], "nor with no answer state at all");
});

test("the four phases read from the reducer's own record for the request on screen", () => {
  assert.deepEqual(phaseOf(ask({}, { InFlight: { draft_len: null } } as AnswerPhase_Serialize)), { kind: "in_flight" });
  assert.deepEqual(phaseOf(ask({}, { Delivered: { via: "daemon (desktop@box)" } } as AnswerPhase_Serialize)), {
    kind: "delivered",
    via: "daemon (desktop@box)",
  });
  assert.deepEqual(phaseOf(ask({}, { Failed: { reason: "no live target", draft_len: 12 } } as AnswerPhase_Serialize)), {
    kind: "failed",
    reason: "no live target",
    draftLen: 12,
  });
  // Another surface got there first: delivered, with the winner named.
  assert.deepEqual(phaseOf(ask({}, { Delivered: { via: "already answered by tui@box" } } as AnswerPhase_Serialize)), {
    kind: "already_answered",
    by: "tui@box",
  });
  // A phase filed under another request is not this question's.
  assert.deepEqual(
    phaseOf({ ...ask({}, { InFlight: { draft_len: null } } as AnswerPhase_Serialize), request: "att-8" }),
    { kind: "none" },
  );
});

test("picking option two sends one pick naming it by label, wherever the cursor is", () => {
  // No cursor move and no Enter: a frame landing between two intents could
  // reorder the options under a counted cursor (#1191). The reducer resolves
  // the label against the options it holds when the pick runs.
  const question = questionFor(sessions(mark()))!;
  const intents = pickIntents(question, ask({ cursor: 2 }), 1);
  assert.deepEqual(commands(intents), ["session_list.select_row", "session_list.select_tab", "session_list.ask.pick"]);
  assert.deepEqual((intents[2] as { Command: [string, unknown] }).Command[1], { request: "att-7", index: 1, label: "production" });
  assert.deepEqual(commands(pickIntents(question, ask({ cursor: 0 }), 1)), commands(intents));
  assert.deepEqual(pickIntents(question, ask(), 3), [], "an index off the list picks nothing");
});

test("a pick sends the label as the frame carried it, not as the banner trims it", () => {
  const long = "x".repeat(120);
  const question = questionFor(sessions(mark({ options: [{ label: long, description: "" }] })))!;
  const intents = pickIntents(question, ask(), 0);
  assert.deepEqual((intents[2] as { Command: [string, unknown] }).Command[1], { request: "att-7", index: 0, label: long });
});

test("a typed answer moves to the composer row, clears it in one step, types, sends", () => {
  const question = questionFor(sessions(mark()))!;
  assert.deepEqual(commands(typedIntents(question, ask({ cursor: 1, free_text_len: 7, focus: "FreeText" }), "qa")), [
    "session_list.select_row",
    "session_list.select_tab",
    "session_list.ask.next",
    "session_list.ask.next",
    "session_list.ask.clear",
    "text:qa",
    "session_list.ask.enter",
  ]);
});

test("a refused step stops the sequence, so Enter is never sent on the wrong row", async () => {
  const question = questionFor(sessions(mark()))!;
  const intents = typedIntents(question, ask({ cursor: 0 }), "qa");
  const sent: string[] = [];
  const refusal: Refusal = { command: "session_list.ask.next", reason: "not from the window" };
  const stopped = await sendInOrder(intents, async (intent) => {
    const name = commands([intent])[0];
    sent.push(name);
    return name === refusal.command ? refusal : null;
  });
  assert.deepEqual(stopped, refusal);
  assert.deepEqual(sent, ["session_list.select_row", "session_list.select_tab", "session_list.ask.next"]);
  assert.ok(!sent.includes("session_list.ask.enter"), "Enter never went out");
});

test("with nothing refused, every intent goes out in order", async () => {
  const question = questionFor(sessions(mark()))!;
  const intents = pickIntents(question, ask({ cursor: 0 }), 1);
  const sent: string[] = [];
  const stopped = await sendInOrder(intents, async (intent) => {
    sent.push(commands([intent])[0]);
    return null;
  });
  assert.equal(stopped, null);
  assert.deepEqual(sent, commands(intents));
});

test("a click on the banner drawn before the question moved on sends nothing", () => {
  // The banner reads the question as painted. A frame moves the question on
  // (att-8, the same labels) before the next paint: the old banner's click
  // names att-7, which the new answer state refuses. After the paint the
  // banner is att-8's, and its click answers att-8.
  const paints: (() => void)[] = [];
  const { current, setCurrent, drawn, dispose } = createRoot((dispose) => {
    const [current, setCurrent] = createSignal<Question>(questionFor(sessions(mark()))!);
    return { current, setCurrent, drawn: drawnQuestion(current, queued(paints)), dispose };
  });
  try {
    paints.splice(0).forEach((paint) => paint());
    assert.equal(drawn().request, "att-7");
    assert.equal(commands(pickIntents(drawn(), ask(), 0)).at(-1), "session_list.ask.pick");

    setCurrent(questionFor(sessions(mark({ request: "att-8" })))!);
    assert.equal(current().request, "att-8", "the frame has moved on");
    assert.equal(drawn().request, "att-7", "the banner has not repainted");
    assert.deepEqual(pickIntents(drawn(), ask({ request: "att-8" }), 0), [], "the old banner's click sends nothing");

    paints.splice(0).forEach((paint) => paint());
    assert.equal(drawn().request, "att-8", "the paint brought the banner to the new question");
    const intents = pickIntents(drawn(), ask({ request: "att-8" }), 0);
    assert.deepEqual((intents[2] as { Command: [string, unknown] }).Command[1], { request: "att-8", index: 0, label: "staging" });
  } finally {
    dispose();
  }
});

/** A paint scheduler over `paints`: a paint waits there until flushed, and its cancel removes it. */
function queued(paints: (() => void)[]) {
  return (paint: () => void) => {
    paints.push(paint);
    return () => {
      const at = paints.indexOf(paint);
      if (at >= 0) paints.splice(at, 1);
    };
  };
}

test("a question that changes under the same request repaints", () => {
  // The repaint is keyed on what the banner shows, not on the request alone:
  // a hook that rewrites its options, title or route under one id must reach
  // the banner, or a click would send a label the person never saw.
  const paints: (() => void)[] = [];
  const { setCurrent, drawn, dispose } = createRoot((dispose) => {
    const [current, setCurrent] = createSignal<Question>(questionFor(sessions(mark()))!);
    return { setCurrent, drawn: drawnQuestion(current, queued(paints)), dispose };
  });
  try {
    paints.splice(0).forEach((paint) => paint());
    assert.deepEqual(drawn().options, ["staging", "production", "local"]);

    setCurrent(questionFor(sessions(mark({ options: [{ label: "canary", description: "" }] })))!);
    assert.equal(paints.length, 1, "a repaint is scheduled for the new labels");
    assert.deepEqual(drawn().options, ["staging", "production", "local"], "not before the paint");
    paints.splice(0).forEach((paint) => paint());
    assert.deepEqual(drawn().options, ["canary"]);

    setCurrent(questionFor(sessions(mark({ options: [{ label: "canary", description: "" }], route: "None" })))!);
    assert.equal(paints.length, 1, "a route change repaints too");
    paints.splice(0).forEach((paint) => paint());
    assert.equal(drawn().answerable, false);

    const painted = drawn();
    setCurrent(questionFor(sessions(mark({ options: [{ label: "canary", description: "" }], route: "None" })))!);
    assert.equal(paints.length, 0, "the same question again schedules nothing");
    // Identity, not just equality: the banner's option rows are keyed off
    // this object's `options`, so a frame that changes only the object must
    // leave the very same array in place, and with it the option elements.
    assert.equal(drawn(), painted, "and the drawn question is the same object");
    assert.equal(drawn().options, painted.options, "with the same options array");
  } finally {
    dispose();
  }
});

test("a pending paint is cancelled by a newer frame and by unmount", () => {
  const paints: (() => void)[] = [];
  const { setCurrent, drawn, dispose } = createRoot((dispose) => {
    const [current, setCurrent] = createSignal<Question>(questionFor(sessions(mark()))!);
    return { setCurrent, drawn: drawnQuestion(current, queued(paints)), dispose };
  });
  paints.splice(0).forEach((paint) => paint());

  setCurrent(questionFor(sessions(mark({ request: "att-8" })))!);
  setCurrent(questionFor(sessions(mark({ request: "att-9" })))!);
  assert.equal(paints.length, 1, "only the newest paint is pending");
  paints.splice(0).forEach((paint) => paint());
  assert.equal(drawn().request, "att-9");

  setCurrent(questionFor(sessions(mark({ request: "att-10" })))!);
  assert.equal(paints.length, 1);
  dispose();
  assert.equal(paints.length, 0, "unmounting cancels the pending paint");
  assert.equal(drawn().request, "att-9", "and nothing paints after it");
});

test("the banner reads props.question only to feed drawnQuestion", () => {
  // Every other read must go through the drawn question: a `props.question`
  // anywhere else is a getter over the frame, which reintroduces the
  // paint-to-click gap while every behavioural test stays green.
  const source = readFileSync(new URL("./answer.tsx", import.meta.url), "utf8")
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/^\s*\/\/.*$/gm, "");
  const reads = source.match(/props\.question/g) ?? [];
  assert.equal(reads.length, 1, `props.question is read ${reads.length} times outside comments`);
  assert.match(source, /drawnQuestion\(\s*\(\) => props\.question,/, "and that one read is the drawnQuestion feed");
});

test("the banner latches on the request while a frame carries no chip and the reducer still points at it", () => {
  // A frame between a scan apply and the next attention merge carries the
  // row with no chip (#1263 closed one source; #1266 makes the banner not
  // depend on it): the banner keeps the question it drew while the reducer's
  // answer state still names that request, so its elements stay in place.
  const shown = questionFor(sessions(mark()))!;
  assert.equal(latchQuestion(null, shown, ask()), shown, "a frame with the question shows it");
  assert.equal(latchQuestion(shown, null, ask()), shown, "a bare frame keeps it while the reducer is on att-7");
  assert.equal(latchQuestion(shown, null, ask({ request: "att-8" })), null, "the reducer moved on: dropped");
  assert.equal(latchQuestion(shown, null, ask({ request: null })), null, "the reducer cleared it: dropped");
  assert.equal(latchQuestion(shown, null, undefined), null, "no answer state at all: dropped");
  const next = questionFor(sessions(mark({ request: "att-8" })))!;
  assert.equal(latchQuestion(shown, next, ask({ request: "att-8" })), next, "a new question replaces it");
  assert.equal(latchQuestion(null, null, ask()), null);
});

test("the banner is keyed on the request id, so the same request keeps one banner across frames", () => {
  const shown = questionFor(sessions(mark()))!;
  const again = questionFor(sessions(mark()))!;
  assert.notEqual(shown, again, "two frames, two objects");
  assert.deepEqual(bannerKeys(shown), ["att-7"]);
  assert.deepEqual(bannerKeys(again), bannerKeys(shown), "the same key, so For keeps the element");
  assert.deepEqual(bannerKeys(questionFor(sessions(mark({ request: "att-8" })))!), ["att-8"], "a new request is a new key");
  assert.deepEqual(bannerKeys(null), [], "no question, no banner");
});
