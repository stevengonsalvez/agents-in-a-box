import { For, Show } from "solid-js";
import type { GitViewView_Serialize } from "../../../ainb-app/bindings/AppState";
import {
  bodyLines,
  fileRows,
  openFile,
  scrollIntent,
  sectionCut,
  selectFileIntent,
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
 * the file's PATH and a wheel sends a line count; the reducer decides what
 * that means and the next frame says what it decided, which is what keeps this
 * window and the terminal showing one review rather than two.
 *
 * It also draws what it does not have. The frame's counters say what the
 * budget left out, and `stale` says the section was withheld whole, so a diff
 * the host has already moved past is never drawn as if it were current.
 */
export function Review(props: Props) {
  const files = () => fileRows(props.gitView);
  const open = () => openFile(props.gitView);
  const cut = () => sectionCut(props.gitView);

  return (
    <section
      class="review"
      aria-label="Review"
      onWheel={(event) => {
        const lines = Math.trunc(event.deltaY / 40);
        if (lines !== 0) props.onChoose(scrollIntent(lines));
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

          <div class="review-body">
            <Show
              when={bodyLines(open()).length > 0}
              fallback={
                <p class="empty">
                  {open()?.binary ? "Binary file, nothing to show" : "Nothing to show for this file"}
                </p>
              }
            >
              <For each={bodyLines(open())}>
                {(line) =>
                  line.kind === "hunk" ? (
                    <p class="review-hunk">
                      {line.header}
                      <Show when={line.hidden > 0}>
                        <span class="review-hidden">{line.hidden} lines above</span>
                      </Show>
                    </p>
                  ) : (
                    <p class="review-row" data-kind={line.row.kind.toLowerCase()}>
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
