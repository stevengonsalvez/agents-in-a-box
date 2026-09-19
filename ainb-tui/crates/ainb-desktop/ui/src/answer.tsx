import { createEffect, createMemo, createSignal, For, on, onCleanup, Show } from "solid-js";
import type { AskState_Serialize } from "../../../ainb-app/bindings/AppState";
import {
  bannerKeys,
  drawnQuestion,
  LATCH_GRACE_MS,
  type Latched,
  latchQuestion,
  phaseOf,
  pickIntents,
  typedIntents,
  type Question,
} from "./answer.ts";
import { label } from "./sessions.ts";
import type { RendererIntent } from "./tabs.ts";

interface Props {
  question: Question;
  /** The frame's `fleet.ask_state`: the cursor, the composer's length, the phases. */
  ask: AskState_Serialize | undefined;
  /** Send `intents` to the reducer, one after another, in order. */
  run(intents: RendererIntent[]): Promise<void>;
}

interface SlotProps {
  /** The frame's question, or null when the frame carries none. */
  question: Question | null;
  ask: AskState_Serialize | undefined;
  run(intents: RendererIntent[]): Promise<void>;
  /** The clock, for tests; `Date.now` in the window. */
  now?(): number;
  /** The grace, for tests; [`LATCH_GRACE_MS`] in the window. */
  grace?: number;
}

/**
 * Where the banner lives: one banner per open request, latched for a short
 * grace after the last frame that carried the question (#1266).
 *
 * Keyed on the request id, so every frame that carries the same request keeps
 * the same banner element, and a new request mounts a fresh banner with a
 * fresh draft. A frame that carries no question does not unmount it inside
 * the grace; when no frame comes to end the grace, a timer does.
 */
export function AnswerSlot(props: SlotProps) {
  const now = () => (props.now ?? Date.now)();
  const grace = () => props.grace ?? LATCH_GRACE_MS;
  // Bumped by the timer so the memo re-reads the clock when no frame does.
  const [wake, setWake] = createSignal(0);
  const latched = createMemo<Latched | null>((kept) => {
    wake();
    return latchQuestion(kept, props.question, now(), grace());
  }, null);
  let timer: ReturnType<typeof setTimeout> | undefined;
  createEffect(() => {
    const held = latched();
    if (timer !== undefined) clearTimeout(timer);
    timer = undefined;
    if (held === null || props.question !== null) return;
    // Kept on grace alone: wake just after it runs out.
    const left = grace() - (now() - held.seenAt);
    timer = setTimeout(() => setWake((n) => n + 1), Math.max(0, left) + 1);
  });
  onCleanup(() => clearTimeout(timer));
  return (
    <For each={bannerKeys(latched()?.question ?? null)}>
      {() => <AnswerBanner question={latched()!.question} ask={props.ask} run={props.run} />}
    </For>
  );
}

/**
 * The attention banner: the question the selected session is blocked on,
 * answered without leaving the window.
 *
 * The frame carries the composer's LENGTH, never its text, so the window keeps
 * its own echo of what was typed. It is kept until the send is delivered, so a
 * failed send leaves the text where the person typed it, beside the reducer's
 * own count of the draft it restored.
 *
 * Everything drawn, and everything a click sends, reads the question as it was
 * painted (`drawnQuestion`), not `props.question` as the frame now has it: the
 * prop is a getter over the frame, and a click that read it named whichever
 * question the frame had moved on to (#1191).
 */
export function AnswerBanner(props: Props) {
  const question = drawnQuestion(
    () => props.question,
    (paint) => {
      const frame = requestAnimationFrame(paint);
      return () => cancelAnimationFrame(frame);
    },
  );
  const [draft, setDraft] = createSignal("");
  const phase = () => phaseOf(props.ask);
  const busy = () => phase().kind === "in_flight";
  // Delivered, here or by another surface: the echo has done its job.
  createEffect(
    on(
      () => phase().kind,
      (kind) => {
        if (kind === "delivered" || kind === "already_answered") setDraft("");
      },
    ),
  );

  const pick = (index: number) => void props.run(pickIntents(question(), props.ask, index));
  const send = () => {
    const text = draft().trim();
    if (text === "") return;
    void props.run(typedIntents(question(), props.ask, text));
  };

  return (
    <section
      class="answer-banner"
      role="region"
      aria-label="Answer"
      data-kind={question().kind}
      data-request={question().request}
    >
      <header>
        <span class="row-kind">{question().kind}</span>
        <span class="answer-title">{question().title}</span>
      </header>
      <Show when={question().detail}>
        <p class="answer-detail">{question().detail}</p>
      </Show>
      <Show when={question().options.length > 0}>
        <div class="answer-options" role="group" aria-label="Options">
          <For each={question().options}>
            {(option, index) => (
              <button
                type="button"
                class="answer-option"
                data-option={index()}
                disabled={busy()}
                onClick={() => pick(index())}
              >
                {index() + 1}. {label(option)}
              </button>
            )}
          </For>
        </div>
      </Show>
      <Show when={question().freeText}>
        <form
          class="answer-composer"
          onSubmit={(event) => {
            event.preventDefault();
            send();
          }}
        >
          <input
            type="text"
            aria-label="Answer"
            placeholder="Type an answer"
            maxlength="2000"
            value={draft()}
            disabled={busy()}
            onInput={(event) => setDraft(event.currentTarget.value)}
          />
          <button type="submit" disabled={busy() || draft().trim() === ""}>
            Send
          </button>
        </form>
      </Show>
      <p class="answer-phase" role="status" data-phase={phase().kind}>
        {phaseLine(phase(), props.ask?.free_text_len ?? 0)}
      </p>
    </section>
  );
}

/** The one line that says what the last send did. */
function phaseLine(phase: ReturnType<typeof phaseOf>, held: number): string {
  switch (phase.kind) {
    case "in_flight":
      return "Sending…";
    case "delivered":
      return `Delivered via ${phase.via}`;
    case "already_answered":
      return `Already answered by ${phase.by}`;
    case "failed":
      return phase.draftLen === null
        ? `Not delivered: ${phase.reason}`
        : `Not delivered: ${phase.reason}. Your ${held}-character draft is kept.`;
    default:
      return "";
  }
}
