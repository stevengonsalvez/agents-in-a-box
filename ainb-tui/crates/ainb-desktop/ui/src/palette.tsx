import { createMemo, createResource, createSignal, For, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import type { SessionsView_Serialize } from "../../../ainb-app/bindings/AppState";
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
            <For each={shown()}>
              {(row, index) => (
                <li>
                  <button
                    type="button"
                    class="palette-row"
                    classList={{ at: index() === at(), inactive: !row.active }}
                    disabled={!row.active}
                    role="option"
                    aria-selected={index() === at()}
                    data-row={row.key}
                    // The input keeps focus, so the press must not take it away.
                    onMouseDown={(event) => event.preventDefault()}
                    onClick={() => choose(row)}
                  >
                    <span class="palette-title">{row.title}</span>
                    <span class="palette-detail">{row.detail}</span>
                    <Show when={row.chord}>
                      <span class="palette-chord">{row.chord}</span>
                    </Show>
                  </button>
                </li>
              )}
            </For>
          </ul>
        </Show>
      </div>
    </div>
  );
}
