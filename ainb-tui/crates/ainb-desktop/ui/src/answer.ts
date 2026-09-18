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
  FleetView_Serialize,
  SessionAttention_Serialize,
  SessionsView_Serialize,
  Session_Serialize,
} from "../../../ainb-app/bindings/AppState";
import { label } from "./sessions.ts";
import type { RendererIntent } from "./tabs.ts";

/** The kinds that block a turn, as `AttentionKind::blocks` has it. */
const BLOCKING: readonly AttentionKind[] = ["Ask", "Wait", "Approve"];

/** `ainb_app::fleet::attention::{APPROVE_LABEL, DENY_LABEL}`. */
export const APPROVE_LABEL = "approve";
export const DENY_LABEL = "deny";

/** How the answer would travel, as far as the window can tell. */
export type Route = "daemon" | "broker" | "pane";

/** The question the banner is showing. */
export interface Question {
  sessionId: string;
  /**
   * The reducer's request id for this question when the window can know it:
   * the daemon's attention id. A pane or broker chip's id is minted from its
   * kind and age, which the frame does not carry.
   */
  request: string | null;
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

/** The daemon's open rows for `session`, by the provider id the host correlated. */
function daemonChips(session: Session_Serialize, fleet: FleetView_Serialize | undefined): SessionAttention_Serialize[] {
  const provider = fleet?.fleet_metadata?.[session.id]?.provider_session_id;
  if (provider === null || provider === undefined) return [];
  return fleet?.daemon_attention?.by_session_id?.[provider] ?? [];
}

/**
 * The question the selected session is blocked on, or `null` when it is not.
 *
 * The daemon's row when it holds one, because only it carries the structured
 * options; otherwise the row's own merged chip, answered through the pane, or
 * through the approve broker for a bare APPROVE, which the reducer offers as
 * exactly `approve` and `deny`.
 */
export function questionFor(
  sessions: SessionsView_Serialize | undefined,
  fleet: FleetView_Serialize | undefined,
): Question | null {
  const session = selectedSession(sessions);
  if (session === undefined) return null;
  const title = label(session.name);
  const daemon = daemonChips(session, fleet).find((chip) => BLOCKING.includes(chip.kind));
  if (daemon !== undefined) {
    return {
      sessionId: session.id,
      request:
        typeof daemon.answerable === "object" && daemon.answerable.Daemon !== undefined
          ? daemon.answerable.Daemon.attention_id
          : null,
      title,
      kind: daemon.kind,
      detail: daemon.detail === null ? null : label(daemon.detail),
      options: daemon.options.map((option) => label(option.label)),
      freeText: true,
      route: "daemon",
    };
  }
  const mark = (session.attention ?? []).find((chip) => BLOCKING.includes(chip.kind));
  if (mark === undefined) return null;
  const broker = mark.kind === "Approve";
  return {
    sessionId: session.id,
    request: null,
    title,
    kind: mark.kind,
    detail: mark.detail === null ? null : label(mark.detail),
    options: broker ? [APPROVE_LABEL, DENY_LABEL] : [],
    freeText: !broker,
    route: broker ? "broker" : "pane",
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
 * Where the reducer's cursor is for `question`: the frame's, unless the frame
 * is pointed at a different request, in which case the retarget the first
 * intent causes puts it back at the top.
 */
function cursorFor(question: Question, ask: AskState_Serialize | undefined): number {
  if (ask === undefined) return 0;
  if (question.request !== null && ask.request !== question.request) return 0;
  return ask.cursor;
}

/** The cursor moves that take the reducer's cursor from `from` to `to`. */
function moves(from: number, to: number): RendererIntent[] {
  const step = to > from ? "session_list.ask.next" : "session_list.ask.previous";
  return Array.from({ length: Math.abs(to - from) }, () => ({ Command: [step, null] }) as RendererIntent);
}

/**
 * The intents that pick option `index` and send it, in order: the reducer's
 * own cursor, moved from where the frame says it is, then Enter.
 */
export function pickIntents(question: Question, ask: AskState_Serialize | undefined, index: number): RendererIntent[] {
  return [
    ...focusIntents(question),
    ...moves(cursorFor(question, ask), index),
    { Command: ["session_list.ask.enter", null] },
  ];
}

/**
 * The intents that type `text` and send it: the cursor to the composer row
 * (after the last option), whatever the reducer's composer already holds
 * cleared, the text typed, then Enter.
 */
export function typedIntents(question: Question, ask: AskState_Serialize | undefined, text: string): RendererIntent[] {
  // What the reducer's composer holds for THIS request; a retarget empties it.
  const held = cursorFor(question, ask) === (ask?.cursor ?? 0) && ask !== undefined ? ask.free_text_len : 0;
  return [
    ...focusIntents(question),
    ...moves(cursorFor(question, ask), question.options.length),
    ...Array.from({ length: held }, () => ({ Command: ["session_list.ask.backspace", null] }) as RendererIntent),
    { Text: text },
    { Command: ["session_list.ask.enter", null] },
  ];
}
