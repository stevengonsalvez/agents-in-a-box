---
name: media-processing
description: Process multimedia files with FFmpeg (video/audio encoding, conversion, streaming, filtering, hardware acceleration), ImageMagick (image manipulation, format conversion, batch processing, effects, composition), and vhs (headless terminal recording for CLI/TUI screenshots, GIFs, and MP4 demos, plus tmux-driven and asciinema capture of live sessions). Use when converting media formats, encoding videos with specific codecs (H.264, H.265, VP9), resizing/cropping images, extracting audio from video, applying filters and effects, optimizing file sizes, creating streaming manifests (HLS/DASH), generating thumbnails, batch processing images, creating composite images, capturing reproducible terminal/TUI/CLI screenshots or demos, recording a CLI command flow or live tmux session (vhs `.tape`, tmux capture-pane, asciinema), or implementing media processing pipelines. Supports 100+ formats, hardware acceleration (NVENC, QSV), and complex filtergraphs.
license: MIT
---

# Media Processing Skill

Process video, audio, and images using FFmpeg and ImageMagick command-line tools for conversion, optimization, streaming, and manipulation tasks.

## When to Use This Skill

Use when:
- Converting media formats (video, audio, images)
- Encoding video with codecs (H.264, H.265, VP9, AV1)
- Processing images (resize, crop, effects, watermarks)
- Extracting audio from video
- Creating streaming manifests (HLS/DASH)
- Generating thumbnails and previews
- Batch processing media files
- Optimizing file sizes and quality
- Applying filters and effects
- Creating composite images or videos
- Capturing reproducible terminal / TUI screenshots, GIFs, or demo MP4s for docs / README assets

## Tool Selection Guide

### FFmpeg: Video/Audio Processing
Use FFmpeg for:
- Video encoding, conversion, transcoding
- Audio extraction, conversion, mixing
- Live streaming (RTMP, HLS, DASH)
- Video filters (scale, crop, rotate, overlay)
- Hardware-accelerated encoding
- Media file inspection (ffprobe)
- Frame extraction, concatenation
- Codec selection and optimization

### ImageMagick: Image Processing
Use ImageMagick for:
- Image format conversion (PNG, JPEG, WebP, GIF)
- Resizing, cropping, transformations
- Batch image processing (mogrify)
- Visual effects (blur, sharpen, sepia)
- Text overlays and watermarks
- Image composition and montages
- Color adjustments, filters
- Thumbnail generation

### vhs: Terminal / TUI Recording
Use vhs for:
- Reproducible terminal screenshots (PNG) for docs / README
- Animated GIFs / MP4s / WebMs of TUI flows
- Headless capture (no real terminal, no display server — CI-friendly)
- Scripted keystroke sequences (Type / Tab / Enter / Escape / Ctrl+c / Sleep)
- Theme-pinned, dimension-pinned output that's identical across machines
- **Not** for: generic screen capture, recording arbitrary GUI apps, or anything outside a PTY

### Decision Matrix

| Task | Tool | Why |
|------|------|-----|
| Video encoding | FFmpeg | Native video codec support |
| Audio extraction | FFmpeg | Direct stream manipulation |
| Image resize | ImageMagick | Optimized for still images |
| Batch images | ImageMagick | mogrify for in-place edits |
| Video thumbnails | FFmpeg | Frame extraction built-in |
| GIF creation | FFmpeg or ImageMagick | FFmpeg for video source, ImageMagick for images |
| Streaming | FFmpeg | Live streaming protocols |
| Image effects | ImageMagick | Rich filter library |
| TUI screenshot / GIF | vhs | Scripted PTY capture, theme-pinned, byte-deterministic |
| Demo video of a CLI flow | vhs | `.tape` file as source of truth — re-runnable, diffable |

## Installation

### macOS
```bash
brew install ffmpeg imagemagick vhs
```

### Ubuntu/Debian
```bash
sudo apt-get install ffmpeg imagemagick
# vhs — install via Go or release tarball, no apt package on most distros
go install github.com/charmbracelet/vhs@latest
# or download from https://github.com/charmbracelet/vhs/releases
```

### Windows
```bash
# Using winget
winget install ffmpeg
winget install ImageMagick.ImageMagick

# Or download binaries
# FFmpeg: https://ffmpeg.org/download.html
# ImageMagick: https://imagemagick.org/script/download.php
# vhs:        https://github.com/charmbracelet/vhs/releases
```

### Verify Installation
```bash
ffmpeg -version
ffprobe -version
magick -version
# or
convert -version
vhs --version       # only if you'll be capturing terminal output
```

## Quick Start Examples

### Video Conversion
```bash
# Convert format (copy streams, fast)
ffmpeg -i input.mkv -c copy output.mp4

# Re-encode with H.264
ffmpeg -i input.avi -c:v libx264 -crf 22 -c:a aac output.mp4

# Resize video to 720p
ffmpeg -i input.mp4 -vf scale=-1:720 -c:a copy output.mp4
```

### Audio Extraction
```bash
# Extract audio (no re-encoding)
ffmpeg -i video.mp4 -vn -c:a copy audio.m4a

# Convert to MP3
ffmpeg -i video.mp4 -vn -q:a 0 audio.mp3
```

### Image Processing
```bash
# Convert format
magick input.png output.jpg

# Resize maintaining aspect ratio
magick input.jpg -resize 800x600 output.jpg

# Create square thumbnail
magick input.jpg -resize 200x200^ -gravity center -extent 200x200 thumb.jpg
```

### Batch Image Resize
```bash
# Resize all JPEGs to 800px width
mogrify -resize 800x -quality 85 *.jpg

# Output to separate directory
mogrify -path ./output -resize 800x600 *.jpg
```

### Video Thumbnail
```bash
# Extract frame at 5 seconds
ffmpeg -ss 00:00:05 -i video.mp4 -vframes 1 -vf scale=320:-1 thumb.jpg
```

### HLS Streaming
```bash
# Generate HLS playlist
ffmpeg -i input.mp4 \
  -c:v libx264 -preset fast -crf 22 -g 48 \
  -c:a aac -b:a 128k \
  -f hls -hls_time 6 -hls_playlist_type vod \
  playlist.m3u8
```

### Image Watermark
```bash
# Add watermark to corner
magick input.jpg watermark.png -gravity southeast \
  -geometry +10+10 -composite output.jpg
```

## Common Workflows

### Optimize Video for Web
```bash
# H.264 with good compression
ffmpeg -i input.mp4 \
  -c:v libx264 -preset slow -crf 23 \
  -c:a aac -b:a 128k \
  -movflags +faststart \
  output.mp4
```

### Create Responsive Images
```bash
# Generate multiple sizes
for size in 320 640 1024 1920; do
  magick input.jpg -resize ${size}x -quality 85 "output-${size}w.jpg"
done
```

### Extract Video Segment
```bash
# From 1:30 to 3:00 (re-encode for precision)
ffmpeg -i input.mp4 -ss 00:01:30 -to 00:03:00 \
  -c:v libx264 -c:a aac output.mp4
```

### Batch Image Optimization
```bash
# Convert PNG to optimized JPEG
mogrify -path ./optimized -format jpg -quality 85 -strip *.png
```

### Video GIF Creation
```bash
# High quality GIF with palette
ffmpeg -i input.mp4 -vf "fps=15,scale=640:-1:flags=lanczos,split[s0][s1];[s0]palettegen[p];[s1][p]paletteuse" output.gif
```

### Image Blur Effect
```bash
# Gaussian blur
magick input.jpg -gaussian-blur 0x8 output.jpg
```

### Incident Evidence Composite

Side-by-side composite with a title bar — useful for pasting into PRs or incident
reports when you have a screenshot plus a chart/second screenshot to compare.

```bash
# Resize both panels to matching height, append horizontally, add title strip
magick screenshot.png  -resize 'x800' -background '#111' -gravity center -extent '450x800' panel_a.png
magick chart.png       -resize 'x800' -background '#fff' -gravity center -extent '1450x800' panel_b.png
magick panel_a.png panel_b.png +append \
  -background '#111' -splice 0x40 \
  -gravity north -pointsize 22 -fill white \
  -annotate +0+8 'Incident · URL · device · timestamp' \
  composite.png
```

### Slideshow MP4 from PNG Glob

Turn a directory of PNGs into a short mp4 (great for evidence clips when you don't
have a real video source — e.g. rrweb/session-replay data).

```bash
# 2 seconds per frame, H.264-compatible output
ffmpeg -y -loglevel error \
  -framerate 1/2 -pattern_type glob -i 'frames/*.png' \
  -vf "scale=trunc(iw/2)*2:trunc(ih/2)*2,format=yuv420p,fps=30" \
  -c:v libx264 -preset veryfast -crf 22 slideshow.mp4
```

**Non-obvious bits:**
- `scale=trunc(iw/2)*2:trunc(ih/2)*2` — H.264 requires **even dimensions**; skip this and the encoder fails silently on odd-width PNGs.
- `format=yuv420p` — without it, players like QuickTime refuse to open.
- `fps=30` keeps the output cadence smooth even when the input is 1/2 fps.

## Terminal Recording with vhs

[vhs](https://github.com/charmbracelet/vhs) (Charm) records a scripted terminal session inside its own virtual PTY and emits a PNG, GIF, MP4, or WebM. No real terminal, no display server, no screen-recording fragility — the `.tape` file is the source of truth, and re-running it yields the same output every time (modulo what the recorded command itself produces).

**Use vhs for:** TUI screenshots in your README/docs, animated demo GIFs of a CLI flow, regression-style snapshots.

**Don't use vhs for:** generic screen capture, GUI app recording, anything where the output isn't text-in-a-terminal.

> **Recording a CLI command flow, a TUI without the cold-paint loader, or driving/asserting a live binary via tmux (or asciinema for a genuinely interactive session)?** Read [`references/terminal-recording.md`](references/terminal-recording.md) — `.tape` recipes for CLI and TUI (`Hide`/`Show`), the tmux `send-keys`/`capture-pane` driver, asciinema, and the field-tested traps (quoted `Output` paths, `send-keys` needs a separate `Enter`, env-isolation, mtime gaps). The minimal recipes below are a quick start.

### Minimal `.tape` — single screenshot

```tape
# home.tape
Output home.png

Set Theme "Catppuccin Mocha"
Set FontSize 16
Set Width 1600
Set Height 900
Set Padding 20
Set Shell bash

Type "./target/debug/myapp tui"
Enter

# vhs Sleep is wall-clock — under-sleeping captures a loading state silently.
# Give a generous margin for cold-start scans (workspace, cache, network).
Sleep 30s

Screenshot home.png

Ctrl+c
Sleep 200ms
```

Run it:

```bash
vhs home.tape       # writes home.png
```

### Animated GIF — keystroke flow

```tape
# demo.tape
Output demo.gif

Set Theme "Catppuccin Mocha"
Set FontSize 16
Set Width 1600
Set Height 900
Set Padding 20

Type "./myapp"
Enter
Sleep 2s

# Navigate the TUI — single nav keys take NO Enter.
Type "j"
Sleep 500ms
Type "j"
Sleep 500ms
Tab
Sleep 1s

# Enter as a separate token only when committing a shell line.
Type "q"
Sleep 500ms
```

### Real `$HOME` vs seeded fixture

Two legitimate modes — pick one per tape and document which:

```tape
# Mode A: real $HOME — authentic state, point-in-time snapshot of your machine
# (good for personal-doc screenshots, README hero shots)
Env AINB_DISABLE_PLUGINS "1"
Type "exec ./target/debug/myapp tui"
Enter
Sleep 75s        # cold scan on a populated $HOME can take ~30-60s

# Mode B: seeded fixture — reproducible across machines, CI-friendly
# (good for tutorials, regression baselines)
Env HOME "/tmp/myapp-screenshot-home"
Type "./scripts/seed-and-run.sh"
Enter
Sleep 8s         # tiny seeded dataset, fast settle
```

### Pitfalls (load-bearing)

- **`Sleep` is real wall-clock.** Under-sleeping silently captures a loading screen. When your binary does a heavy cold scan, time it once in a real tmux session and add ~30% margin to your sleep budget.
- **`Type` does not press Enter.** Append `Enter` as a separate directive only when committing a shell line. Single-char TUI nav keys (`j`, `q`, `i`, etc.) take NO Enter.
- **Theme names must match vhs's bundled list exactly** and stay quoted: `Set Theme "Catppuccin Mocha"`, not `Set Theme catppuccin-mocha`.
- **`Width` / `Height` are pixels, not cells.** `FontSize` drives effective columns × rows; a 1600×900 frame at FontSize 16 yields roughly 160 cols × 35 rows on most fonts.
- **`exec` your binary** to avoid leaving the wrapping shell prompt in the recording. `Type "exec ./bin"` instead of `Type "./bin"` makes the shell get replaced by your process.
- **`Env HOME ...` does NOT inherit `~/.config` etc.** If you point at a fresh dir, seed it first; if you point at the real `$HOME`, accept that the screenshot is per-contributor.
- **First-time `vhs` install on macOS** pulls `ttyd` + `ffmpeg` as dependencies — be patient on the first `brew install vhs`.

### Combine with the existing skill

vhs is just an output node — once you have a `.png` / `.gif` / `.mp4`, the rest of this skill kicks in:

```bash
# 1. Record the TUI to a GIF.
vhs demo.tape                                          # → demo.gif

# 2. Convert to MP4 for a README that doesn't auto-play GIFs.
ffmpeg -i demo.gif -movflags +faststart -pix_fmt yuv420p demo.mp4

# 3. Take a still frame for a thumbnail.
ffmpeg -i demo.mp4 -ss 00:00:02 -vframes 1 thumb.png

# 4. Side-by-side with a "before" screenshot using the Incident Composite recipe.
magick before.png demo-thumb.png +append composite.png
```

## Advanced Techniques

### Multi-Pass Video Encoding
```bash
# Pass 1 (analysis)
ffmpeg -y -i input.mkv -c:v libx264 -b:v 2600k -pass 1 -an -f null /dev/null

# Pass 2 (encoding)
ffmpeg -i input.mkv -c:v libx264 -b:v 2600k -pass 2 -c:a aac output.mp4
```

### Hardware-Accelerated Encoding
```bash
# NVIDIA NVENC
ffmpeg -hwaccel cuda -i input.mp4 -c:v h264_nvenc -preset fast -crf 22 output.mp4

# Intel QuickSync
ffmpeg -hwaccel qsv -c:v h264_qsv -i input.mp4 -c:v h264_qsv output.mp4
```

### Complex Image Pipeline
```bash
# Resize, crop, border, adjust
magick input.jpg \
  -resize 1000x1000^ \
  -gravity center \
  -crop 1000x1000+0+0 +repage \
  -bordercolor black -border 5x5 \
  -brightness-contrast 5x10 \
  -quality 90 \
  output.jpg
```

### Video Filter Chains
```bash
# Scale, denoise, watermark
ffmpeg -i video.mp4 -i logo.png \
  -filter_complex "[0:v]scale=1280:720,hqdn3d[v];[v][1:v]overlay=10:10" \
  -c:a copy output.mp4
```

### Animated GIF from Images
```bash
# Create with delay
magick -delay 100 -loop 0 frame*.png animated.gif

# Optimize size
magick animated.gif -fuzz 5% -layers Optimize optimized.gif
```

## Media Analysis

### Inspect Video Properties
```bash
# Detailed JSON output
ffprobe -v quiet -print_format json -show_format -show_streams input.mp4

# Get resolution
ffprobe -v error -select_streams v:0 \
  -show_entries stream=width,height \
  -of csv=s=x:p=0 input.mp4
```

### Image Information
```bash
# Basic info
identify image.jpg

# Detailed format
identify -verbose image.jpg

# Custom format
identify -format "%f: %wx%h %b\n" image.jpg
```

## Performance Tips

1. **Use CRF for quality control** - Better than bitrate for video
2. **Copy streams when possible** - Avoid re-encoding with `-c copy`
3. **Hardware acceleration** - GPU encoding 5-10x faster
4. **Appropriate presets** - Balance speed vs compression
5. **Batch with mogrify** - In-place image processing
6. **Strip metadata** - Reduce file size with `-strip`
7. **Progressive JPEG** - Better web loading with `-interlace Plane`
8. **Limit memory** - Prevent crashes on large batches
9. **Test on samples** - Verify settings before batch
10. **Parallel processing** - Use GNU Parallel for multiple files

## Reference Documentation

Detailed guides in `references/`:

- **ffmpeg-encoding.md** - Video/audio codecs, quality optimization, hardware acceleration
- **ffmpeg-streaming.md** - HLS/DASH, live streaming, adaptive bitrate
- **ffmpeg-filters.md** - Video/audio filters, complex filtergraphs
- **imagemagick-editing.md** - Format conversion, effects, transformations
- **imagemagick-batch.md** - Batch processing, mogrify, parallel operations
- **format-compatibility.md** - Format support, codec recommendations

For vhs `.tape` syntax beyond the patterns above, see the upstream reference: https://github.com/charmbracelet/vhs#vhs-command-reference

## Common Parameters

### FFmpeg Video
- `-c:v` - Video codec (libx264, libx265, libvpx-vp9)
- `-crf` - Quality (0-51, lower=better, 23=default)
- `-preset` - Speed/compression (ultrafast to veryslow)
- `-b:v` - Video bitrate (e.g., 2M, 2500k)
- `-vf` - Video filters

### FFmpeg Audio
- `-c:a` - Audio codec (aac, mp3, opus)
- `-b:a` - Audio bitrate (e.g., 128k, 192k)
- `-ar` - Sample rate (44100, 48000)

### ImageMagick Geometry
- `800x600` - Fit within (maintains aspect)
- `800x600!` - Force exact size
- `800x600^` - Fill (may crop)
- `800x` - Width only
- `x600` - Height only
- `50%` - Scale percentage

## Troubleshooting

**FFmpeg "Unknown encoder"**
```bash
# Check available encoders
ffmpeg -encoders | grep h264

# Install codec libraries
sudo apt-get install libx264-dev libx265-dev
```

**ImageMagick "not authorized"**
```bash
# Edit policy file
sudo nano /etc/ImageMagick-7/policy.xml
# Change <policy domain="coder" rights="none" pattern="PDF" />
# to <policy domain="coder" rights="read|write" pattern="PDF" />
```

**Memory errors**
```bash
# Limit memory usage
ffmpeg -threads 4 input.mp4 output.mp4
magick -limit memory 2GB -limit map 4GB input.jpg output.jpg
```

## Related Skills

- **PostHog / rrweb session replays** — if the "video" source is actually a
  PostHog session recording (or any rrweb event stream), don't reach for ffmpeg
  first. rrweb replays are event logs, not pixel streams; `rrvideo` npm packages
  are broken (`@rrweb/rrvideo` is 404, unscoped `rrvideo@0.2.1` silently
  produces nothing). See the `posthog-replay-analysis` skill for the decode
  recipe — you can extract network timelines, DOM snapshots, and console logs
  directly from the rrweb events, then render them into a chart + mp4 slideshow
  using the patterns above (*Incident Evidence Composite* and *Slideshow MP4*).

## Resources

- FFmpeg: https://ffmpeg.org/documentation.html
- FFmpeg Wiki: https://trac.ffmpeg.org/
- ImageMagick: https://imagemagick.org/
- ImageMagick Usage: https://imagemagick.org/Usage/
