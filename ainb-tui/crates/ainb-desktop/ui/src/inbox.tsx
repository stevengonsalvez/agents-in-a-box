import { For, Show } from "solid-js";
import type { InboxView_Serialize } from "../../../ainb-app/bindings/AppState";
import { inboxView, MARK_ALL_READ } from "./inbox.ts";
import type { RendererIntent } from "./tabs.ts";

interface Props {
  /** Section 16, or undefined until the host frames it. */
  inbox: InboxView_Serialize | undefined;
  /** The sweep was chosen: send it to the reducer. */
  onChoose(intent: RendererIntent): void;
  /** Leave the page: the reducer walks back to the session list. */
  onClose(): void;
}

/**
 * The desktop inbox (D3p-c): the daemon's inbox, newest first, drawn from
 * section 16 and nothing else.
 *
 * It keeps no state of its own. Which entries are unread, the count and the
 * cuts are the frame's; the one control is the whole-inbox sweep, sent as a
 * command, and the next frame is what says it landed. There is no per-row
 * control because the daemon's verb has none.
 */
export function Inbox(props: Props) {
  const page = () => inboxView(props.inbox);
  return (
    <section class="inbox" aria-label="Inbox" data-state={page().state}>
      <header class="inbox-head">
        <h2>Inbox</h2>
        <button
          type="button"
          class="inbox-sweep"
          disabled={!page().canMarkAllRead}
          onClick={() => props.onChoose(MARK_ALL_READ)}
        >
          Mark all read
        </button>
        <button type="button" class="inbox-close" aria-label="Close the inbox" onClick={() => props.onClose()}>
          ×
        </button>
      </header>
      <Show when={page().status}>
        {(line) => (
          <p class="inbox-status" role="status">
            {line()}
          </p>
        )}
      </Show>
      <Show when={page().cut}>
        {(line) => (
          <p class="inbox-cut" role="status">
            {line()}
          </p>
        )}
      </Show>
      <Show
        when={page().rows.length > 0}
        fallback={<p class="empty">{page().state === "empty" ? "Nothing in the inbox" : ""}</p>}
      >
        <ol class="inbox-rows">
          <For each={page().rows}>
            {(row) => (
              <li class="inbox-row" classList={{ unread: row.unread }} data-entry={row.id}>
                <span class="inbox-label">{row.label}</span>
                <span class="inbox-summary">{row.summary}</span>
                <time class="inbox-when">{row.when}</time>
              </li>
            )}
          </For>
        </ol>
      </Show>
    </section>
  );
}
