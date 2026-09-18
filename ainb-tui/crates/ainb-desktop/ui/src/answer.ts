// The question the banner answers, and the intents that answer it. Projections
// and sequences only: `answer.tsx` draws and sends what these return.
//
// Nothing here decides an answer. The window moves the reducer's own cursor
// with `session_list.ask.previous`/`next`, types through `Intent::Text` into
// the reducer's own composer, and sends with `session_list.ask.enter`, so the
// verified send (`AskState::send`) is the only send there is.

import type {
  AnswerPhase_Serialize,
  AskState_Serialize,
  AttentionKind,
  SessionsView_Serialize,
  Session_Serialize,
} from "../../../ainb-app/bindings/AppState";
import { label } from "./sessions.ts";
import type { RendererIntent } from "./tabs.ts";

/** The kinds that block a turn, as `AttentionKind::blocks` has it. */
const BLOCKING: readonly AttentionKind[] = ["Ask", "Wait", "Approve"];

/** How the answer would travel, as far as the window can tell. */
export type Route = "daemon" | "broker" | "pane";

/** The question the banner is showing. */
export interface Question {
  sessionId: string;
  /** The reducer's request id for this chip, as `fleet.ask_state.request` names it. */
  request: string;
  title: string;
  kind: AttentionKind;
  detail: string | null;
  /** The labels a pick chooses between, in the reducer's cursor order. */
  options: string[];
  /**
   * Whether a typed answer can be sent. Not over the approve broker: a parked
   * permission request reads `approve` or `deny` and refuses anything else, so
   * a composer there would offer a send that cannot land.
   */
  freeText: boolean;
  /** Whether any answer can be delivered from here at all. */
  answerable: boolean;
  route: Route;
}

/** What the last send for the question on screen did. */
export type PhaseView =
  | { kind: "none" }
  | { kind: "in_flight" }
  | { kind: "delivered"; via: string }
  | { kind: "already_answered"; by: string }
  | { kind: "failed"; reason: string; draftLen: number | null };

/** The session list row the reducer has selected, when it is a session. */
export function selectedSession(view: SessionsView_Serialize | undefined): Session_Serialize | undefined {
  if (view === undefined) return undefined;
  const workspace = view.selected_workspace_index;
  const session = view.selected_session_index;
  if (workspace === null || session === null || view.shell_selected) return undefined;
  return view.workspaces[workspace]?.sessions[session];
}

/**
 * The question the selected session is blocked on, or `null` when it is not.
 *
 * The row's own first blocking chip, which is the chip the reducer answers
 * (`selected_blocking` walks the same merged list in the same order), with its
 * request id, options and route read off it. The daemon's rows are NOT read
 * here: they are in wire order, and with two open questions on one session the
 * window would have shown one and the reducer answered the other.
 */
export function questionFor(sessions: SessionsView_Serialize | undefined): Question | null {
  const session = selectedSession(sessions);
  if (session === undefined) return null;
  const mark = (session.attention ?? []).find((chip) => BLOCKING.includes(chip.kind));
  if (mark === undefined) return null;
  const route: Route = mark.route === "Daemon" ? "daemon" : mark.route === "Broker" ? "broker" : "pane";
  return {
    sessionId: session.id,
    request: mark.request,
    title: label(session.name),
    kind: mark.kind,
    detail: mark.detail === null ? null : label(mark.detail),
    options: mark.options.map((option) => label(option.label)),
    // Not over the broker: it reads `approve` or `deny` and refuses anything
    // else. Not where nothing can deliver an answer at all.
    freeText: mark.route === "Daemon" || mark.route === "Pane",
    answerable: mark.route !== "None",
    route,
  };
}

/** What the reducer recorded for the request it is pointed at. */
export function phaseOf(ask: AskState_Serialize | undefined): PhaseView {
  if (ask === undefined || ask.request === null) return { kind: "none" };
  const found = ask.phases.find(([request]) => request === ask.request);
  if (found === undefined) return { kind: "none" };
  const phase: AnswerPhase_Serialize = found[1];
  if ("InFlight" in phase) return { kind: "in_flight" };
  if (phase.Delivered !== undefined) {
    // Another surface got there first: the session has its reply, so this
    // reads as delivered, with the winner named.
    const already = /^already answered by (.+)$/.exec(phase.Delivered.via);
    if (already) return { kind: "already_answered", by: label(already[1]) };
    return { kind: "delivered", via: label(phase.Delivered.via) };
  }
  if (phase.Failed !== undefined) {
    return { kind: "failed", reason: label(phase.Failed.reason), draftLen: phase.Failed.draft_len };
  }
  return { kind: "none" };
}

/** Put the reducer on the question: the row selected, the `ask` pane showing. */
function focusIntents(question: Question): RendererIntent[] {
  return [
    { Command: ["session_list.select_row", { target: { session: question.sessionId }, open: false }] },
    { Command: ["session_list.select_tab", { tab: "Ask" }] },
  ];
}

/**
 * Whether the frame is pointed at `question`: the reducer has retargeted its
 * answer state to this chip, so its cursor and composer are this chip's.
 *
 * Anything else is refused, not corrected. Moving a cursor counted off another
 * request's frame, or typing where another request's options sit, is exactly
 * how a window answers a question the person did not read.
 */
export function pointedAt(question: Question, ask: AskState_Serialize | undefined): ask is AskState_Serialize {
  return ask !== undefined && ask.request === question.request;
}

/** The cursor moves that take the reducer's cursor from `from` to `to`. */
function moves(from: number, to: number): RendererIntent[] {
  const step = to > from ? "session_list.ask.next" : "session_list.ask.previous";
  return Array.from({ length: Math.abs(to - from) }, () => ({ Command: [step, null] }) as RendererIntent);
}

/**
 * The intents that pick option `index` and send it, in order: the reducer's own
 * cursor, moved from where the frame says it is, then Enter. None at all when
 * the frame is not pointed at this question.
 */
export function pickIntents(question: Question, ask: AskState_Serialize | undefined, index: number): RendererIntent[] {
  if (!pointedAt(question, ask) || !question.answerable) return [];
  return [...focusIntents(question), ...moves(ask.cursor, index), { Command: ["session_list.ask.enter", null] }];
}

/**
 * The intents that type `text` and send it: the cursor to the composer row
 * (after the last option), the reducer's composer cleared in one step, the text
 * typed, then Enter. None at all when the frame is not pointed at this
 * question, or when this route takes no typed answer.
 */
export function typedIntents(question: Question, ask: AskState_Serialize | undefined, text: string): RendererIntent[] {
  if (!pointedAt(question, ask) || !question.freeText) return [];
  return [
    ...focusIntents(question),
    ...moves(ask.cursor, question.options.length),
    { Command: ["session_list.ask.clear", null] },
    { Text: text },
    { Command: ["session_list.ask.enter", null] },
  ];
}
