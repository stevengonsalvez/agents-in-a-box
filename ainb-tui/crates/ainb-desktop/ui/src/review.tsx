import { createEffect, For, Show } from "solid-js";
import type { GitViewView_Serialize } from "../../../ainb-app/bindings/AppState";
import {
  bodyLines,
  fileRows,
  gitView,
  scrollIntent,
  sectionCut,
  selectFileIntent,
  wheelRows,
  WITHHELD,
} from "./review.ts";
import type { RendererIntent } from "./tabs.ts";

interface Props {
  gitView: GitViewView_Serialize | undefined;
  /** The section was withheld for being over the frame ceiling. */
  stale: boolean;
  /** A file or a scroll was chosen: send it to the reducer. */
  onChoose(intent: RendererIntent): void;
}

/**
 * The review tab: the changed files down the side, the open file's hunks in
 * the body, drawn from the framed git view.
 *
 * The tab keeps no selection and no scroll offset of its own. A click sends
 * the file's PATH and a wheel sends a ROW COUNT; the reducer decides what that
 * means, the next frame says what it decided, and an effect puts the body
 * where that frame says. The browser's own scrolling is refused, because two
 * sources of one offset is how this window and the terminal end up showing
 * different rows.
 *
 * It also draws what it does not have. The frame's counters say what the
 * budget left out, and `stale` says the section was withheld whole, so a diff
 * the host has already moved past is never drawn as if it were current.
 */
export function Review(props: Props) {
  const files = () => fileRows(props.gitView);
  const body = () => bodyLines(props.gitView);
  const cut = () => sectionCut(props.gitView);
  const scroll = () => gitView(props.gitView)?.review_ui.scroll ?? 0;

  let bodyElement: HTMLDivElement | undefined;
  /** Pixels a wheel has sent that have not yet made a whole row. */
  let pending = 0;

  // The offset is the reducer's, so the body is put where the frame says
  // rather than wherever the last wheel left it: the terminal and this window
  // show the same rows, and #1221 can window them by the same number.
  createEffect(() => {
    const first = scroll();
    // Read the body so a new frame's rows re-run this after they are drawn.
    body();
    const element = bodyElement;
    if (element === undefined) return;
    const row = element.querySelector<HTMLElement>(`[data-vrow="${first}"]`);
    element.scrollTop = row === undefined || row === null ? 0 : row.offsetTop - element.offsetTop;
  });

  return (
    <section
      class="review"
      aria-label="Review"
      onWheel={(event) => {
        // The browser must not scroll the body as well: one source of the
        // offset, and it is the reducer.
        event.preventDefault();
        const step = wheelRows(pending, event);
        pending = step.pending;
        if (step.rows !== 0) props.onChoose(scrollIntent(step.rows));
      }}
    >
      <Show when={props.stale}>
        <p class="review-withheld" role="status">
          {WITHHELD}
        </p>
      </Show>
      <Show when={cut()}>{(line) => <p class="review-cut" role="status">{line()}</p>}</Show>

      <Show
        when={files().length > 0}
        fallback={<p class="empty">{props.gitView ? "No changes to review" : "Loading the review"}</p>}
      >
        <div class="review-panes">
          <ul class="review-files">
            <For each={files()}>
              {(file) => (
                <li>
                  <button
                    type="button"
                    class="review-file"
                    classList={{ open: file.open }}
                    data-file={file.path}
                    aria-current={file.open ? "true" : undefined}
                    onClick={() => props.onChoose(selectFileIntent(file.path))}
                  >
                    <span class="review-path">{file.path}</span>
                    <span class="review-counts">
                      +{file.insertions} −{file.deletions}
                    </span>
                    <Show when={file.cut}>{(note) => <span class="review-file-cut">{note()}</span>}</Show>
                  </button>
                </li>
              )}
            </For>
          </ul>

          <div class="review-body" ref={bodyElement}>
            <Show
              when={body().length > 0}
              fallback={<p class="empty">Nothing to show for these changes</p>}
            >
              <For each={body()}>
                {(line) =>
                  line.kind === "file" ? (
                    <p
                      class="review-file-head"
                      classList={{ open: line.open }}
                      data-head={line.file.path}
                      data-vrow={line.index}
                    >
                      <span class="review-path">{line.file.path}</span>
                      <span class="review-counts">
                        +{line.file.insertions} −{line.file.deletions}
                      </span>
                      <span class="review-status">{line.file.status}</span>
                      <Show when={line.file.binary}>
                        <span class="review-hidden">binary, nothing to show</span>
                      </Show>
                    </p>
                  ) : line.kind === "hunk" ? (
                    <p class="review-hunk" classList={{ current: line.current }}>
                      {line.header}
                    </p>
                  ) : line.kind === "expand" ? (
                    <p class="review-expand" data-vrow={line.index}>
                      <span class="review-hidden">{line.hidden} lines hidden</span>
                    </p>
                  ) : (
                    <p class="review-row" data-kind={line.row.kind.toLowerCase()} data-vrow={line.index}>
                      <span class="review-lineno">{line.row.old_lineno ?? ""}</span>
                      <span class="review-lineno">{line.row.new_lineno ?? ""}</span>
                      <span class="review-text">{line.row.raw}</span>
                    </p>
                  )
                }
              </For>
            </Show>
          </div>
        </div>
      </Show>
    </section>
  );
}
