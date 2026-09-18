// The banner's question, the phases it reads, and the intents it sends.

import assert from "node:assert/strict";
import { test } from "node:test";
import type {
  AnswerPhase_Serialize,
  AskState_Serialize,
  FleetView_Serialize,
  SessionsView_Serialize,
} from "../../../ainb-app/bindings/AppState";
import { phaseOf, pickIntents, questionFor, typedIntents } from "./answer.ts";

function sessions(attention: { kind: string; detail: string | null }[] = []): SessionsView_Serialize {
  return {
    workspaces: [{ name: "repo", sessions: [{ id: "u-1", name: "api", attention }] }],
    selected_workspace_index: 0,
    selected_session_index: 0,
    shell_selected: false,
  } as unknown as SessionsView_Serialize;
}

function fleet(chips: unknown[] = []): FleetView_Serialize {
  return {
    fleet_metadata: { "u-1": { provider_session_id: "p-1" } },
    daemon_attention: { by_session_id: { "p-1": chips }, all: {}, reachable: true, error: null, not_running: false },
  } as unknown as FleetView_Serialize;
}

const daemonAsk = {
  kind: "Ask",
  since_ms: 1,
  source: "Daemon",
  detail: "Which environment?",
  options: [
    { label: "staging", description: "" },
    { label: "production", description: "" },
    { label: "local", description: "" },
  ],
  answerable: { Daemon: { attention_id: "att-7" } },
};

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

test("the daemon's row is the question, with its options and its attention id", () => {
  const question = questionFor(sessions([{ kind: "Ask", detail: "x" }]), fleet([daemonAsk]))!;
  assert.equal(question.route, "daemon");
  assert.equal(question.request, "att-7");
  assert.deepEqual(question.options, ["staging", "production", "local"]);
  assert.equal(question.freeText, true);
});

test("a bare approve offers exactly approve and deny, and no composer", () => {
  const question = questionFor(sessions([{ kind: "Approve", detail: "rm -rf build" }]), fleet())!;
  assert.equal(question.route, "broker");
  assert.deepEqual(question.options, ["approve", "deny"]);
  assert.equal(question.freeText, false, "the broker refuses anything but the two labels");
});

test("nothing blocking, nothing selected: no banner", () => {
  assert.equal(questionFor(sessions([{ kind: "Done", detail: null }]), fleet()), null);
  const none = { ...sessions(), selected_session_index: null } as unknown as SessionsView_Serialize;
  assert.equal(questionFor(none, fleet([daemonAsk])), null);
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

test("picking option two moves the reducer's cursor from where it is, then sends", () => {
  const question = questionFor(sessions(), fleet([daemonAsk]))!;
  assert.deepEqual(commands(pickIntents(question, ask({ cursor: 0 }), 1)), [
    "session_list.select_row",
    "session_list.select_tab",
    "session_list.ask.next",
    "session_list.ask.enter",
  ]);
  assert.deepEqual(commands(pickIntents(question, ask({ cursor: 2 }), 1)).slice(2), [
    "session_list.ask.previous",
    "session_list.ask.enter",
  ]);
  // A frame pointed at another request: the retarget resets the cursor, so
  // the moves count from the top.
  assert.deepEqual(commands(pickIntents(question, ask({ request: "att-other", cursor: 2 }), 1)).slice(2), [
    "session_list.ask.next",
    "session_list.ask.enter",
  ]);
});

test("a typed answer moves to the composer row, clears what the reducer holds, types, sends", () => {
  const question = questionFor(sessions(), fleet([daemonAsk]))!;
  assert.deepEqual(commands(typedIntents(question, ask({ cursor: 1, free_text_len: 2, focus: "FreeText" }), "qa")), [
    "session_list.select_row",
    "session_list.select_tab",
    "session_list.ask.next",
    "session_list.ask.next",
    "session_list.ask.backspace",
    "session_list.ask.backspace",
    "text:qa",
    "session_list.ask.enter",
  ]);
});
