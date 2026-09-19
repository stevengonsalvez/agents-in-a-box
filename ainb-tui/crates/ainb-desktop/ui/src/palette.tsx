import { createMemo, createResource, createSelector, createSignal, For, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import type { SessionsView_Serialize } from "../../../ainb-app/bindings/AppState";
import { keyedList, sameKeys } from "./keyed.ts";
import { commandRows, rank, sessionRows, stepRow, type PaletteEntry, type PaletteRow } from "./palette.ts";
import type { RendererIntent } from "./tabs.ts";

interface Props {
  /** The sessions frame the session rows come from. */
  sessions: SessionsView_Serialize | undefined;
  /** A row was chosen: dispatch its intent. */
  onChoose(intent: RendererIntent): void;
  /** Esc, a click outside, or a chosen row: put focus back where it was. */
  onClose(): void;
}

/**
 * The command palette: every command the host offers, plus the live sessions,
 * over one query.
 *
 * The command rows are fetched each time it opens rather than held, because
 * whether a row runs in the state as it stands is part of the answer. A row
 * that would be refused is drawn greyed rather than hidden, so the list does
 * not shift under the user as the reducer moves.
 */
export function Palette(props: Props) {
  const [query, setQuery] = createSignal("");
  const [at, setAt] = createSignal(0);
  // Fetched once per mount; the palette is mounted only while it is open.
  const [entries] = createResource(async () => {
    try {
      return await invoke<PaletteEntry[]>("palette");
    } catch (error) {
      console.warn("the palette's commands were not listed", error);
      return [] as PaletteEntry[];
    }
  });

  const rows = createMemo(() => [...sessionRows(props.sessions), ...commandRows(entries() ?? [])]);
  const shown = createMemo(() => rank(rows(), query()));
  // The list is drawn from the rows' keys, not the row objects. Every frame
  // the host sends builds new row objects, and `For` keys by identity, so a
  // list of objects re-created every row on every frame (#1267): a click
  // could land on a button that had just been replaced. Keys are strings,
  // equal from one frame to the next, so an unchanged row keeps its node and
  // only its text is patched from `byKey`.
  // Through `keyedList`, so two rows that somehow name one key still draw as
  // two rows, each with its own text.
  const list = createMemo(() => keyedList(shown(), (row) => row.key));
  const keys = createMemo(() => list().keys, [], { equals: sameKeys });
  // Only the row the cursor leaves and the row it reaches re-run.
  const isAt = createSelector(() => list().keys[at()]);
  const choose = (row: PaletteRow | undefined) => {
    // A row the reducer would refuse now is drawn, so the list does not shift
    // under the user, but choosing it would do nothing and say nothing.
    if (row === undefined || !row.active) return;
    props.onChoose(row.intent);
    props.onClose();
  };

  const onKey = (event: KeyboardEvent) => {
    switch (event.key) {
      case "ArrowDown":
      case "ArrowUp":
        event.preventDefault();
        setAt(stepRow(shown().length, at(), event.key === "ArrowDown" ? 1 : -1));
        return;
      case "Enter":
        event.preventDefault();
        choose(shown()[at()]);
        return;
      case "Escape":
        event.preventDefault();
        props.onClose();
    }
  };

  return (
    // The backdrop closes on a click that lands outside the panel.
    <div class="palette-backdrop" onClick={props.onClose}>
      <div class="palette" role="dialog" aria-label="Command palette" onClick={(event) => event.stopPropagation()}>
        <input
          class="palette-query"
          type="text"
          placeholder="Run a command or open a session"
          aria-label="Command palette"
          autofocus
          maxlength="200"
          ref={(element) => queueMicrotask(() => element.focus())}
          value={query()}
          onInput={(event) => {
            setQuery(event.currentTarget.value);
            setAt(0);
          }}
          onKeyDown={onKey}
        />
        <Show when={shown().length > 0} fallback={<p class="empty">Nothing matches</p>}>
          <ul class="palette-rows" role="listbox">
            <For each={keys()}>
              {(key) => {
                // The row as the latest frame has it. Undefined only for the
                // moment between a frame dropping this key and `For` removing
                // the node, when nothing reads it.
                const row = () => list().byKey.get(key);
                const active = () => row()?.active ?? false;
                return (
                  <li>
                    <button
                      type="button"
                      class="palette-row"
                      classList={{ at: isAt(key), inactive: !active() }}
                      disabled={!active()}
                      role="option"
                      aria-selected={isAt(key)}
                      data-row={row()?.key}
                      // The input keeps focus, so the press must not take it away.
                      onMouseDown={(event) => event.preventDefault()}
                      // The row as it is when the click lands, not as it was
                      // when the node was made.
                      onClick={() => choose(row())}
                    >
                      <span class="palette-title">{row()?.title}</span>
                      <span class="palette-detail">{row()?.detail}</span>
                      <Show when={row()?.chord}>
                        <span class="palette-chord">{row()?.chord}</span>
                      </Show>
                    </button>
                  </li>
                );
              }}
            </For>
          </ul>
        </Show>
      </div>
    </div>
  );
}

