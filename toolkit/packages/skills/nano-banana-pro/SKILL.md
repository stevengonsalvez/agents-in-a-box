---
name: nano-banana-pro
description: Generate or edit images via Gemini 3 Pro Image (Nano Banana Pro).
version: 1.0.0
---

# Nano Banana Pro (Gemini 3 Pro Image)

Generate or edit images using Google's Gemini 3 Pro Image API.

## Requirements

- **uv** (Python package manager) - Install: `brew install uv`
- **GEMINI_API_KEY** environment variable

## Usage

### Generate New Image

```bash
uv run {baseDir}/scripts/generate_image.py --prompt "your image description" --filename "output.png" --resolution 1K
```

### Edit Single Image

```bash
uv run {baseDir}/scripts/generate_image.py --prompt "edit instructions" --filename "output.png" -i "/path/in.png" --resolution 2K
```

### Multi-Image Composition (up to 14 images)

```bash
uv run {baseDir}/scripts/generate_image.py --prompt "combine these into one scene" --filename "output.png" -i img1.png -i img2.png -i img3.png
```

## Configuration

**API Key** (in order of precedence):
1. `--api-key` argument
2. `GEMINI_API_KEY` environment variable

## Options

| Flag | Description |
|------|-------------|
| `-p, --prompt` | Image description/prompt (required) |
| `-f, --filename` | Output filename (required) |
| `-i, --input-image` | Input image for editing (can specify multiple) |
| `-r, --resolution` | Output resolution: 1K, 2K, 4K (default: 1K) |
| `-k, --api-key` | Gemini API key |

## Notes

- Use timestamps in filenames: `yyyy-mm-dd-hh-mm-ss-name.png`
- The script outputs a `MEDIA:` line with the saved file path
- Do not read the image back; report the saved path only
- When editing, resolution auto-detects from input image dimensions

## When to Use

- User asks to generate an image
- User wants to edit/modify an existing image
- User wants to combine multiple images
- Any AI image generation task
