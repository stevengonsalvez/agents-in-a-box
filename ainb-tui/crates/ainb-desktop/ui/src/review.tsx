import { createEffect, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import type { GitViewView_Serialize } from "../../../ainb-app/bindings/AppState";
import {
  bodyLines,
  fileRows,
  gitView,
  keyRows,
  ROW_PX,
  scrollIntent,
  sectionCut,
  segments,
  selectFileIntent,
  wheelRows,
  windowLines,
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
  const scrollCut = () => gitView(props.gitView)?.review_ui.scroll_cut === true;

  let bodyElement: HTMLDivElement | undefined;
  /** Pixels a wheel has sent that have not yet made a whole row. */
  let pending = 0;
  /**
   * Rows the body can show at once, for a page key, a page wheel and the
   * window drawn below. It is a signal because the drawing reads it: a plain
   * read of `clientHeight` would fix the window at whatever the body measured
   * when it first mounted.
   */
  const [rowsPerPage, setRowsPerPage] = createSignal(40);
  onMount(() => {
    const measure = () => setRowsPerPage(Math.max(Math.floor((bodyElement?.clientHeight ?? 0) / ROW_PX), 1));
    measure();
    // The body, not the window: a pane that grows because the sidebar
    // collapsed or the banner cleared changes how many rows fit without the
    // window ever being resized.
    if (typeof ResizeObserver === "function" && bodyElement !== undefined) {
      const observer = new ResizeObserver(measure);
      observer.observe(bodyElement);
      onCleanup(() => observer.disconnect());
      return;
    }
    window.addEventListener("resize", measure);
    onCleanup(() => window.removeEventListener("resize", measure));
  });

  /**
   * What is drawn: the rows around the reducer's offset, never the whole body.
   *
   * The body overflows nothing (`.review-body` hides it) and the offset is the
   * frame's, so the window has no scroll position of its own to keep and
   * nothing below the last drawn row to reach. Drawing all 4,000 rows at the
   * frame's bound cost a real window fifty seconds (#1221).
   */
  const drawn = () => windowLines(body(), scroll(), rowsPerPage());

  // The offset is the reducer's, so the body is put where the frame says
  // rather than wherever the last wheel left it: the terminal and this window
  // show the same rows, and #1221 can window them by the same number.
  createEffect(() => {
    const first = scroll();
    // Read the drawn rows so a new frame's window re-runs this after it is
    // drawn.
    drawn();
    const element = bodyElement;
    if (element === undefined) return;
    const row = element.querySelector<HTMLElement>(`[data-vrow="${first}"]`);
    // A row the frame does not carry leaves the body where it is: jumping to
    // the top would lose the place over a frame that simply cut something.
    if (row === null) return;
    element.scrollTop = row.offsetTop - element.offsetTop;
  });

  const move = (rows: number) => {
    if (rows !== 0) props.onChoose(scrollIntent(rows));
  };

  return (
    <section
      class="review"
      aria-label="Review"
      onWheel={(event) => {
        // The browser must not scroll the body as well: one source of the
        // offset, and it is the reducer.
        event.preventDefault();
        const step = wheelRows(pending, event, rowsPerPage());
        pending = step.pending;
        move(step.rows);
      }}
    >
      <Show when={props.stale}>
        <p class="review-withheld" role="status">
          {WITHHELD}
        </p>
      </Show>
      <Show when={cut()}>{(line) => <p class="review-cut" role="status">{line()}</p>}</Show>
      <Show when={scrollCut()}>
        <p class="review-cut" role="status">
          The row the terminal is on was not sent; this is the nearest one that was.
        </p>
      </Show>

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

          {/* The body takes focus and answers the keys the terminal's review
              answers, because the wheel is refused and a keyboard is the only
              other way to move an offset the reducer owns. */}
          <div
            class="review-body"
            ref={bodyElement}
            tabIndex={0}
            role="region"
            aria-label="Diff"
            onKeyDown={(event) => {
              const rows = keyRows(event.key, rowsPerPage());
              if (rows === null) return;
              event.preventDefault();
              move(rows);
            }}
          >
            <Show
              when={drawn().length > 0}
              fallback={<p class="empty">Nothing to show for these changes</p>}
            >
              {/* The window is a page of a longer diff, so the rows carry their
                  place in the whole of it: without the count and the index, a
                  reader is told the diff is eighty rows long. */}
              <div role="table" aria-rowcount={body().length} aria-label="Diff rows">
                <For each={drawn()}>
                {(line) =>
                  line.kind === "file" ? (
                    <p
                      class="review-file-head"
                      classList={{ open: line.open }}
                      data-head={line.file.path}
                      data-vrow={line.index}
                      role="row"
                      aria-rowindex={line.index + 1}
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
                    <p class="review-expand" data-vrow={line.index} role="row" aria-rowindex={line.index + 1}>
                      <span class="review-hidden">{line.hidden} lines hidden</span>
                    </p>
                  ) : (
                    <p
                      class="review-row"
                      data-kind={line.row.kind.toLowerCase()}
                      data-vrow={line.index}
                      role="row"
                      aria-rowindex={line.index + 1}
                    >
                      <span class="review-lineno">{line.row.old_lineno ?? ""}</span>
                      <span class="review-lineno">{line.row.new_lineno ?? ""}</span>
                      <span class="review-text">
                        <For each={segments(line.row)}>
                          {(run) => (
                            <Show when={run.emphasis} fallback={run.text}>
                              <span class="review-emph">{run.text}</span>
                            </Show>
                          )}
                        </For>
                      </span>
                    </p>
                  )
                }
                </For>
              </div>
            </Show>
          </div>
        </div>
      </Show>
    </section>
  );
}
