#!/bin/bash
SD="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
set -u
B=$SD/target/release/spike2
OUT=$SD/out
mkdir -p "$OUT"
BACKENDS="vt100 alacritty wezterm wezterm14"
SIZES="120x40 40x20"
FIXTURES="f1-altscreen f2-mouse f3-widechars f4-osc8 f5-oscstatus f6-scroll f7-decstbm f8-widthprobe"

echo "== A. feed fidelity: control mode vs direct pty =="
printf '%-14s %-7s %-13s %-6s %-11s %s\n' fixture size bytes_pty/tmux raw chunks feed_vs_pty
for fx in $FIXTURES; do
  for sz in $SIZES; do
    C=${sz%x*}; R=${sz#*x}
    S=$SD/fixtures/$fx.sh
    timeout 120 "$B" capture-pty  "$S" "$C" "$R" "$OUT/$fx-$sz-pty.bin"  >"$OUT/$fx-$sz-pty.meta" 2>&1
    timeout 120 "$B" capture-tmux ainb-spike2 s2 "$S" "$C" "$R" "$OUT/$fx-$sz-tmux" >"$OUT/$fx-$sz-tmux.meta" 2>&1
    pb=$(stat -c%s "$OUT/$fx-$sz-pty.bin" 2>/dev/null || echo 0)
    tb=$(stat -c%s "$OUT/$fx-$sz-tmux.bin" 2>/dev/null || echo 0)
    pc=$(wc -l < "$OUT/$fx-$sz-pty.chunks"); tc=$(wc -l < "$OUT/$fx-$sz-tmux.chunks")
    if cmp -s "$OUT/$fx-$sz-pty.bin" "$OUT/$fx-$sz-tmux.bin"; then raw=SAME; else raw=DIFF; fi
    res=""
    for be in $BACKENDS; do
      # Each side replays its own real arrival boundaries, so a parser that
      # mishandles a split escape or a split grapheme shows up as a diff.
      timeout 120 "$B" snapshot "$OUT/$fx-$sz-pty.bin"  "$C" "$R" "$be" 1000 "$OUT/$fx-$sz-pty.chunks"  > "$OUT/$fx-$sz-$be-pty.snap"
      timeout 120 "$B" snapshot "$OUT/$fx-$sz-tmux.bin" "$C" "$R" "$be" 1000 "$OUT/$fx-$sz-tmux.chunks" > "$OUT/$fx-$sz-$be-tmux.snap"
      if cmp -s "$OUT/$fx-$sz-$be-pty.snap" "$OUT/$fx-$sz-$be-tmux.snap"; then
        res="$res $be=EQ"
      else
        n=$(diff "$OUT/$fx-$sz-$be-pty.snap" "$OUT/$fx-$sz-$be-tmux.snap" | grep -c '^[<>]')
        res="$res $be=DIFF($n)"
        diff -u "$OUT/$fx-$sz-$be-pty.snap" "$OUT/$fx-$sz-$be-tmux.snap" > "$OUT/$fx-$sz-$be.feeddiff"
      fi
    done
    printf '%-14s %-7s %-13s %-6s %-11s %s\n' "$fx" "$sz" "$pb/$tb" "$raw" "$pc/$tc" "$res"
  done
done

echo
echo "== B. crate fidelity: emulator grid vs tmux capture-pane =="
printf '%-14s %-7s %s\n' fixture size "mismatched rows per crate"
for fx in $FIXTURES; do
  for sz in $SIZES; do
    C=${sz%x*}; R=${sz#*x}
    timeout 120 bash "$SD/capture-ref.sh" "$fx" "$C" "$R" "$OUT/$fx-$sz-tmuxref.txt"
    res=""
    for be in $BACKENDS; do
      m=$(python3 "$SD/compare-ref.py" "$OUT/$fx-$sz-$be-tmux.snap" "$OUT/$fx-$sz-tmuxref.txt" "$R" \
            > "$OUT/$fx-$sz-$be.refdiff"; head -1 "$OUT/$fx-$sz-$be.refdiff" | sed 's/.*mismatched=//')
      res="$res $be=$m"
    done
    printf '%-14s %-7s %s\n' "$fx" "$sz" "$res"
  done
done
