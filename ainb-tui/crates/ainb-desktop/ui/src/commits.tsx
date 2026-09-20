import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import type { GitViewView_Serialize } from "../../../ainb-app/bindings/AppState";
import {
  commitCount,
  commitRows,
  commitsCut,
  commitWindow,
  scrollFor,
  selectCommitIntent,
} from "./commits.ts";
import { keyedList, sameKeys } from "./keyed.ts";
import { gitView, keyRows, MAX_PAGE_ROWS, ROW_PX, scrollIntent, wheelRows, WITHHELD } from "./review.ts";
import type { RendererIntent } from "./tabs.ts";

interface Props {
  gitView: GitViewView_Serialize | undefined;
  /** The section was withheld for being over the frame ceiling. */
  stale: boolean;
  onChoose: (intent: RendererIntent) => void;
}

/**
 * The Commits tab: the branch's commits as the reducer holds them.
 *
 * The reducer owns the selection, and on this tab the selection IS the scroll
 * (`components/git_view.rs:258-276`): a wheel and the arrow keys send
 * `git_view.scroll`, the reducer moves its selected commit, and the next frame
 * says where it landed. This window keeps no cursor of its own, so the
 * terminal and the window are never on different commits.
 *
 * A click names the commit's short hash, so a click resolved against one frame
 * acts on the same commit after the list moved, and a click on a commit the
 * list no longer carries does nothing.
 */
export function Commits(props: Props) {
  const rows = createMemo(() => commitRows(props.gitView));
  const cut = () => commitsCut(props.gitView);
  const selected = () => gitView(props.gitView)?.selected_commit_index ?? 0;

  let listElement: HTMLDivElement | undefined;
  /** Pixels a wheel has sent that have not yet made a whole row. */
  let pending = 0;
  /**
   * Rows the list can show at once. The window is the outer bound, not the
   * box: a box that is not bounded by its own row measures the content it just
   * drew (`review.tsx`, #1221), and the cap holds whatever is left.
   */
  const [rowsPerPage, setRowsPerPage] = createSignal(40);
  onMount(() => {
    const measure = () =>
      setRowsPerPage(
        Math.min(
          Math.max(Math.floor(Math.min(listElement?.clientHeight ?? 0, window.innerHeight) / ROW_PX), 1),
          MAX_PAGE_ROWS,
        ),
      );
    measure();
    if (typeof ResizeObserver === "function" && listElement !== undefined) {
      const observer = new ResizeObserver(measure);
      observer.observe(listElement);
      onCleanup(() => observer.disconnect());
      return;
    }
    window.addEventListener("resize", measure);
    onCleanup(() => window.removeEventListener("resize", measure));
  });

  // Keyed by the commit's own hash, so a frame that changes nothing patches
  // the rows instead of re-creating them and a click cannot land on a node
  // that has just been replaced (#1267).
  const drawn = createMemo(() =>
    keyedList(commitWindow(rows(), selected(), rowsPerPage()), (row) => row.sha),
  );
  const drawnKeys = createMemo(() => drawn().keys, [], { equals: sameKeys });

  // The selected commit is kept in view, the way the terminal's list keeps it
  // there: the list moves only when the selection has left it, because here
  // the selection is a cursor and not a scroll offset.
  createEffect(() => {
    const at = selected();
    drawnKeys();
    const element = listElement;
    if (element === undefined) return;
    const row = element.querySelector<HTMLElement>(`[data-index="${at}"]`);
    if (row === null) return;
    const to = scrollFor(element, { top: row.offsetTop - element.offsetTop, height: row.offsetHeight });
    if (to !== undefined) element.scrollTop = to;
  });

  const move = (lines: number) => {
    if (lines !== 0) props.onChoose(scrollIntent(lines));
  };

  return (
    <section class="commits">
      <Show when={props.stale}>
        <p class="review-withheld" role="status">
          {WITHHELD}
        </p>
      </Show>
      <Show when={cut()}>{(line) => <p class="review-cut" role="status">{line()}</p>}</Show>

      <div
        class="commit-list"
        ref={listElement}
        tabIndex={0}
        role="region"
        aria-label="Commits"
        onWheel={(event) => {
          // The reducer owns the offset, so the wheel is translated into rows
          // and sent; nothing here scrolls the box itself.
          event.preventDefault();
          const step = wheelRows(pending, event, rowsPerPage());
          pending = step.pending;
          move(step.rows);
        }}
        onKeyDown={(event) => {
          const lines = keyRows(event.key, rowsPerPage());
          if (lines === null) return;
          event.preventDefault();
          move(lines);
        }}
      >
        <Show
          when={rows().length > 0}
          fallback={<p class="empty">No commits on this branch yet</p>}
        >
          <div role="table" aria-rowcount={commitCount(props.gitView)} aria-label="Commit rows">
            <For each={drawnKeys()}>
              {(key) => {
                // The row as the latest frame has it, read when it is drawn
                // and again when it is clicked.
                const row = () => drawn().byKey.get(key);
                return (
                  <button
                    type="button"
                    class="commit-row"
                    classList={{ selected: row()?.selected }}
                    data-sha={row()?.sha}
                    data-index={row()?.index}
                    role="row"
                    aria-rowindex={(row()?.index ?? 0) + 1}
                    aria-current={row()?.selected ? "true" : undefined}
                    onClick={() => {
                      const sha = row()?.sha;
                      if (sha !== undefined) props.onChoose(selectCommitIntent(sha));
                    }}
                  >
                    <span class="commit-sha">{row()?.sha}</span>
                    <span class="commit-message">{row()?.message}</span>
                    <span class="commit-author">{row()?.author}</span>
                    <span class="commit-date">{row()?.date}</span>
                  </button>
                );
              }}
            </For>
          </div>
        </Show>
      </div>
    </section>
  );
}
