# shellcheck shell=bash
# D3 desktop review: a diff written by a separate process into a session's
# worktree reaches the open window as a framed `git_view` section, read from
# the renderer's own telemetry rather than its pixels, and the window is
# subscribed to that section from its first batch.
#
# Opening the review tab and drawing its rows is the wdio review journey's leg
# (crates/ainb-desktop/e2e/specs/review.e2e.js): a plain window has no driver
# to click with, exactly as d2-board leaves answering to answer.e2e.js. What
# this node proves is the half a driver cannot fake: that the section the tab
# draws from is subscribed and applied in a real window, over a real diff.

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="a diff written into a session's worktree by a separate process is carried by the git_view section the open desktop window subscribes to and applies, and the window says so in its own telemetry"

# The diff the fixture writes: past the frame's MAX_ROWS_TOTAL of 4,000 rows
# together, so what crosses is a cut one rather than a toy change.
DIFF_FILES=12
DIFF_LINES=900

# write_diff: DIFF_FILES changed files in the fixture session's worktree,
# written by this process rather than by the window or the agent.
write_diff() {
  local file line body
  for ((file = 0; file < DIFF_FILES; file += 1)); do
    body=""
    for ((line = 0; line < DIFF_LINES; line += 1)); do
      body+="pub fn generated_${file}_${line}() -> usize { ${line} * ${file} + 7 }"$'\n'
    done
    printf '%s' "$body" >"$FIXTURE_CWD/generated_${file}.rs"
  done
}

# changed_files: what git says changed in the worktree, counted by a process
# that is not the window.
changed_files() {
  git -C "$FIXTURE_CWD" status --porcelain 2>/dev/null | grep -c . || true
}

# diff_bytes: the diff's size on disk, which is the floor this node records.
diff_bytes() {
  du -sb "$FIXTURE_CWD" 2>/dev/null | cut -f1
}

# applied_git_view: whether the renderer has applied a batch carrying the
# section the review tab draws from.
applied_git_view() {
  grep -q '"git_view"' <<<"$(applied_sections)"
}

scenario() {
  desktop_ready || return

  fixture_session || { check "the CLI seeded a session before the window opened" false; return; }

  # Written before the window opens, so what the window carries is a diff it
  # never watched appear.
  write_diff
  observe "changed files in the worktree: $(changed_files), bytes on disk: $(diff_bytes)"
  check "a separate process wrote $DIFF_FILES changed files into the worktree" \
    test "$(changed_files)" = "$DIFF_FILES"

  if ! start_desktop; then
    check "the desktop window applied a frame batch within 90 s" false
    [[ -s "$PROOF_WORLD/desktop.stderr" ]] && observe "window stderr: $(tail -3 "$PROOF_WORLD/desktop.stderr")"
    return
  fi
  observe "sections in the first batch the renderer applied: $(applied_sections)"

  # The subscription is the thing a review tab cannot work without: a section
  # absent from it applies nothing at all, whatever the reducer framed.
  check "the window subscribes to and applies the git_view section" \
    wait_for 60 applied_git_view

  # The window is still live with the diff in place: a section that crossed
  # once and then took the window down would pass the check above and fail a
  # person.
  check "the window is still applying batches with the diff in place" \
    wait_for 60 sessions_at_least 1

  if [[ -s "$DESKTOP_LOG" ]]; then
    tail -n 400 "$DESKTOP_LOG" | redact_host >"$NODE_DIR/desktop-log.txt"
    CAPTURES+=("desktop-log.txt")
  fi
}
