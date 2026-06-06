# Frame-truth: prove the EXACT outcome, not "something rendered"

The whole point of the visual gate. A recording that merely proves the terminal was not blank proves nothing. You must extract real frames and confirm they show the *correct* content for the journey.

## Anti-pattern (rejected)
- "The mp4 is 40KB so the screen showed something." ❌
- "OCR found one non-empty line." ❌
- "vhs exited 0." ❌ (it renders loading screens happily)

## The recipe

```
.tape ──vhs──▶ <name>.gif + <name>.mp4 ──ffmpeg──▶ frames/<name>-NNN.png
                                                          │
                                              ├─ tesseract OCR  (pre-filter: do `expect` strings appear?)
                                              └─ READ the PNG    (the real assertion: is the content correct?)
```

`scripts/verify-journey.sh` does the first three boxes and the OCR pre-filter, then prints a checklist. The last box — reading the PNG with judgement — is yours and is mandatory.

## Capturing the right frame
- Put `Screenshot {OUT}/<name>.png` in the tape immediately after the `Sleep` that follows the key — that PNG is the deterministic "settled" frame. Prefer it.
- Also keep the mp4; `verify-journey.sh` extracts the LAST frame (most settled) plus a couple of evenly-spaced frames via ffmpeg as backup:
  ```bash
  ffmpeg -y -i <name>.mp4 -vf "select='eq(n\,0)+gt(scene\,0)'" -vsync vfr frames/<name>-%03d.png   # scene cuts
  ffmpeg -y -sseof -0.3 -i <name>.mp4 -frames:v 1 frames/<name>-last.png                            # final frame
  ```
- For a tighter crop on a region (e.g. just the sidebar or the changed line), use imagemagick after extraction:
  ```bash
  magick frames/<name>-last.png -crop 360x900+0+0 +repage frames/<name>-sidebar.png   # left column
  ```

## OCR pre-filter (best-effort, not the gate)
```bash
tesseract frames/<name>-last.png stdout 2>/dev/null | rg -i "Code Review|skill\.rs|\+57"
```
OCR on TUI frames is noisy (box-drawing, ligatures, color). Treat a hit as supporting evidence and a miss as "look harder", never as the verdict. The verdict is the read.

## Reading the frame (the actual assertion)
Open each frame (Read tool on the PNG). For each journey's `assert`, confirm:
- The right TEXT is present and correct (file paths, counts, code content, counters).
- The right COLOR/STATE (green added row vs red removed row; brighter word-emphasis vs flat tint; chevron `∨` vs `›`; which tab is active).
- The screen MATCHES THE DESIGN (layout, gutter, bars) — compare against the plan's End-State mockup or a reference screenshot.
- The NEGATIVE for exit journeys (the surface is GONE after Esc).

If any is wrong → the journey fails → fix loop (G4).

## Output layout
```
<out>/
  tapes/<name>.tape          # rendered from template
  <name>.gif  <name>.mp4     # the recordings (gif feeds the explainer)
  frames/<name>-*.png        # extracted/cropped frames you READ
  <name>.ocr.txt             # OCR dump (pre-filter only)
```
