// The question the banner answers, and the intents that answer it. Projections
// and sequences only: `answer.tsx` draws and sends what these return.
//
// Nothing here decides an answer. A pick names its option by label in one
// `session_list.ask.pick`, which the reducer resolves against the options it
// holds; a typed answer goes through `Intent::Text` into the reducer's own
// composer and is sent with `session_list.ask.enter`. The verified send
// (`AskState::send`) is the only send there is.

import { type Accessor, createEffect, createMemo, createSignal, on, onCleanup } from "solid-js";
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
  /**
   * The labels a pick chooses between, in the reducer's cursor order and as
   * the frame carries them: a pick sends one back verbatim, so the reducer's
   * own match finds it. Trimmed for display only where they are drawn.
   */
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

/**
 * The session list row the reducer has selected, when it is a session.
 *
 * Resolved by the id the frame names against the rows it carries (#1180): the
 * frame holds only the rows its filter shows, so an index into the reducer's
 * full list would name the wrong one. No selection when the frame names none,
 * or names a row it does not carry.
 */
export function selectedSession(view: SessionsView_Serialize | undefined): Session_Serialize | undefined {
  const id = view?.selected_session_id;
  if (view === undefined || id === null || id === undefined || view.shell_selected) return undefined;
  for (const workspace of view.workspaces) {
    const session = workspace.sessions.find((row) => row.id === id);
    if (session !== undefined) return session;
  }
  return undefined;
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
    options: mark.options.map((option) => option.label),
    // Not over the broker: it reads `approve` or `deny` and refuses anything
    // else. Not where nothing can deliver an answer at all.
    freeText: mark.route === "Daemon" || mark.route === "Pane",
    answerable: mark.route !== "None",
    route,
  };
}

/**
 * The question the banner shows, latched on its request: the frame's question
 * when the frame carries one; otherwise the one `kept` from the last frame,
 * for as long as the reducer's answer state still names its request.
 *
 * A frame can carry the selected row with no chip for a moment (a scan apply
 * before the next attention merge was one source, #1263). A banner that
 * followed every frame unmounted then, and the next frame mounted a new one
 * with new elements under a click. The reducer's `ask_state.request` is not
 * written by such a frame, so it says whether the question is still the one
 * being answered: moved on, cleared, or absent means the banner goes.
 */
export function latchQuestion(
  kept: Question | null,
  current: Question | null,
  ask: AskState_Serialize | undefined,
): Question | null {
  if (current !== null) return current;
  if (kept !== null && ask !== undefined && ask.request === kept.request) return kept;
  return null;
}

/**
 * What the banner list is keyed on: the request id, one banner per open
 * request. Strings key by value in `For`, so every frame that carries the
 * same request keeps the same banner element, and a new request mounts a new
 * one, with a fresh draft.
 */
export function bannerKeys(question: Question | null): string[] {
  return question === null ? [] : [question.request];
}

/**
 * What a banner shows of a question, as one string: a repaint is due when
 * this changes. The request, the title, whether it can be answered, and the
 * labels in order; a hook that rewrites its options under one id reaches the
 * banner, and a frame that carries the same question again does not repaint.
 */
function drawnDigest(question: Question): string {
  return [question.request, question.title, String(question.answerable), ...question.options].join("\u001f");
}

/**
 * The question a banner has drawn, one paint behind `current`.
 *
 * `current` is the frame's question as of now. A click handler that read it
 * at click time answered whatever question the frame had moved on to, with
 * that question's own request id, so the reducer's request check passed for a
 * question the person never saw (#1191). The drawn question follows the frame
 * only after the next paint (`schedule`; `requestAnimationFrame` in the
 * window), keyed on what the banner shows (`drawnDigest`): between a frame
 * that moved the question on and the paint that shows it, a click still names
 * the old request, which `pointedAt` refuses against the new answer state.
 *
 * `schedule` returns the paint's cancel. A newer frame cancels the paint still
 * pending, so only the newest question paints, and the banner's unmount
 * cancels the last one, so nothing paints into a banner that is gone.
 */
export function drawnQuestion(
  current: Accessor<Question>,
  schedule: (paint: () => void) => () => void,
): Accessor<Question> {
  const [drawn, setDrawn] = createSignal(current());
  // A memo, not a bare accessor: `on` reruns whenever what it tracks writes,
  // and every frame writes a new question object. The memo notifies only when
  // the digest string itself changes.
  const digest = createMemo(() => drawnDigest(current()));
  createEffect(
    on(digest, () => {
      const question = current();
      onCleanup(schedule(() => setDrawn(question)));
    }),
  );
  return drawn;
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
 * The intents that pick option `index` and send it: the reducer put on the
 * question, then ONE pick naming the option by its index, with the label the
 * person read there as the check. The reducer verifies both against the
 * options it holds when the pick runs, so a frame landing between two intents
 * cannot move a counted cursor onto another option (#1191), and two labels
 * that scrub alike on a frame are still told apart (#1248). The pick names the request the person read, and the reducer
 * refuses it once the question has moved on. None at all when the frame is
 * not pointed at this question, or when `index` names no option it offers.
 */
export function pickIntents(question: Question, ask: AskState_Serialize | undefined, index: number): RendererIntent[] {
  if (!pointedAt(question, ask) || !question.answerable) return [];
  const label = question.options[index];
  if (label === undefined) return [];
  return [
    ...focusIntents(question),
    { Command: ["session_list.ask.pick", { request: question.request, index, label }] },
  ];
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

/**
 * An intent the host did not apply, with the row it would have run and why
 * (`ainb_desktop::intent::Refusal`).
 */
export interface Refusal {
  command: string;
  reason: string;
}

/**
 * Send `intents` in order, one at a time, and STOP at the first one the host
 * refused, returning it.
 *
 * A typed answer is a sequence: put the cursor on the composer row, clear it,
 * type, then Enter. Each step is applied before the next is sent. If a step is
 * refused and the sequence carries on, Enter still fires, on whatever the
 * reducer's cursor is on: the wrong answer, sent as if a person typed it.
 */
export async function sendInOrder(
  intents: RendererIntent[],
  send: (intent: RendererIntent) => Promise<Refusal | null>,
): Promise<Refusal | null> {
  for (const intent of intents) {
    const refusal = await send(intent);
    if (refusal !== null && refusal !== undefined) return refusal;
  }
  return null;
}
