import { createMemo, For, Show } from "solid-js";
import type { TranscriptView } from "./acp.ts";
import { keyedList, sameKeys } from "./keyed.ts";

interface Props {
  /** The Fleet session key the card was opened for. */
  sessionKey: string;
  /** The frame's transcript for it, or `null` until the host has framed one. */
  view: TranscriptView | null;
  /** Close the card: the host drops the transcript and frames the default. */
  onClose(): void;
}

/**
 * The ACP card: an ACP session's transcript, standing where its terminal would
 * be, because it has no tmux pane to show.
 *
 * Each chunk is drawn with its kind (the agent's message, its thinking, a tool
 * call, a plan, a permission it asked for), as the daemon's own classifier
 * renders it and scrubbed on the host. The prompt echo and the usage report,
 * which that classifier leaves silent, arrive with the host's card text
 * (`acp_card_text`): what the operator asked, and the context and cost.
 *
 * Read-only: answering a permission goes through the board's attention list,
 * as every other question does.
 */
export function AcpCard(props: Props) {
  return (
    <section class="acp-card" aria-label="ACP transcript">
      <header>
        <span class="acp-title">{props.sessionKey}</span>
        <button type="button" class="acp-close" aria-label="Close transcript" onClick={() => props.onClose()}>
          ×
        </button>
      </header>
      <Show when={props.view} fallback={<p class="empty">Opening the transcript</p>}>
        {(view) => {
          // Drawn by key, not by object identity (#1267). `transcriptView`
          // builds new chunk objects on every read, and `For` keys by
          // identity, so every line of a transcript was re-created whenever
          // the Fleet frame moved, losing a selection mid-read. The key is the
          // chunk's order, which does not change once the daemon has sent it.
          const chunks = createMemo(() => keyedList(view().chunks, (chunk) => String(chunk.key)));
          const chunkKeys = createMemo(() => chunks().keys, [], { equals: sameKeys });
          return (
          <>
            <Show when={view().status}>
              {(line) => (
                <p class="acp-status" role="status">
                  {line()}
                </p>
              )}
            </Show>
            <Show when={view().startsPartWay}>
              <p class="acp-status">Earlier chunks are not shown</p>
            </Show>
            <Show
              when={view().chunks.length > 0}
              fallback={<p class="empty">Nothing in this run yet</p>}
            >
              <ol class="acp-chunks">
                <For each={chunkKeys()}>
                  {(key) => {
                    const chunk = () => chunks().byKey.get(key);
                    return (
                      <li class="acp-chunk" data-kind={chunk()?.kind} data-chunk={key}>
                        <span class="acp-kind">{chunk()?.label}</span>
                        <span class="acp-body">
                          {chunk()?.body}
                          <Show when={chunk()?.truncated}>
                            <span class="acp-cut"> (cut)</span>
                          </Show>
                        </span>
                      </li>
                    );
                  }}
                </For>
              </ol>
            </Show>
          </>
          );
        }}
      </Show>
    </section>
  );
}
