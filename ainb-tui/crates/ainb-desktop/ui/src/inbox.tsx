import { createMemo, For, Show } from "solid-js";
import type { InboxView_Serialize } from "../../../ainb-app/bindings/AppState";
import { inboxView, MARK_ALL_READ, ROW_PX, scrollIntents } from "./inbox.ts";
import { keyedList, sameKeys } from "./keyed.ts";
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
 * It keeps no state of its own. Which entries are unread, the count, the cuts
 * and which row the window starts at are the frame's; the one control is the
 * whole-inbox sweep, sent as a command, and the next frame is what says it
 * landed. There is no per-row control because the daemon's verb has none.
 *
 * The offset is the reducer's, as the review tab's is: the wheel sends
 * `inbox.scroll_up` / `inbox.scroll_down` and the browser's own scrolling of
 * the list is refused, so the terminal and this window draw one window of one
 * inbox rather than two.
 */
export function Inbox(props: Props) {
  const page = createMemo(() => inboxView(props.inbox));
  // Drawn by key, not by object identity (#1267): every frame builds new row
  // objects, so an unchanged entry keeps its node and only its text patches.
  const rows = createMemo(() => keyedList(page().rows, (row) => row.id));
  const rowKeys = createMemo(() => rows().keys, [], { equals: sameKeys });
  /** Pixels a wheel has sent that have not yet made a whole row. */
  let pending = 0;
  return (
    <section
      class="inbox"
      aria-label="Inbox"
      data-state={page().state}
      onWheel={(event) => {
        // One source for the offset, and it is the reducer.
        event.preventDefault();
        const total = pending + event.deltaY * (event.deltaMode === 1 ? ROW_PX : 1);
        const moved = Math.trunc(total / ROW_PX);
        pending = total - moved * ROW_PX;
        for (const intent of scrollIntents(moved)) props.onChoose(intent);
      }}
    >
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
      <Show when={page().scrolled}>
        <p class="inbox-cut" role="status">
          Earlier entries are above; scroll up to see them.
        </p>
      </Show>
      <Show when={page().cut}>
        {(line) => (
          <p class="inbox-cut" role="status">
            {line()}
          </p>
        )}
      </Show>
      <Show when={page().rows.length > 0} fallback={<Show when={page().state === "empty"}>
        <p class="empty">Nothing in the inbox</p>
      </Show>}>
        <ol class="inbox-rows">
          <For each={rowKeys()}>
            {(key) => {
              const row = () => rows().byKey.get(key);
              return (
                <li class="inbox-row" classList={{ unread: row()?.unread }} data-entry={row()?.id}>
                  <span class="inbox-label">{row()?.label}</span>
                  <span class="inbox-summary">{row()?.summary}</span>
                  <time class="inbox-when">{row()?.when}</time>
                </li>
              );
            }}
          </For>
        </ol>
      </Show>
    </section>
  );
}
