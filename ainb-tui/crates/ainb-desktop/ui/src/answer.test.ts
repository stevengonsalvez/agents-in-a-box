// The banner's question, the phases it reads, and the intents it sends.

import assert from "node:assert/strict";
import { test } from "node:test";
import type {
  AnswerPhase_Serialize,
  AskState_Serialize,
  AttentionMark_Serialize,
  SessionsView_Serialize,
} from "../../../ainb-app/bindings/AppState";
import { phaseOf, pickIntents, questionFor, type Refusal, selectedSession, sendInOrder, typedIntents } from "./answer.ts";

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
  assert.deepEqual((intents[2] as { Command: [string, unknown] }).Command[1], { label: "production" });
  assert.deepEqual(commands(pickIntents(question, ask({ cursor: 0 }), 1)), commands(intents));
  assert.deepEqual(pickIntents(question, ask(), 3), [], "an index off the list picks nothing");
});

test("a pick sends the label as the frame carried it, not as the banner trims it", () => {
  const long = "x".repeat(120);
  const question = questionFor(sessions(mark({ options: [{ label: long, description: "" }] })))!;
  const intents = pickIntents(question, ask(), 0);
  assert.deepEqual((intents[2] as { Command: [string, unknown] }).Command[1], { label: long });
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
