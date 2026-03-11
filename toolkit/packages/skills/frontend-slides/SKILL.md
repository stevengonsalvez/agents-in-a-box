---
name: frontend-slides
description: |
  Generate zero-dependency HTML presentations with keyboard navigation,
  print CSS, responsive design, and speaker notes. Creates a single
  self-contained HTML file — no build step, no framework, no CDN.

  Use when: (1) Creating slide decks or presentations, (2) Building
  pitch decks, (3) Making technical talks, (4) User asks for slides
  or a presentation, (5) Quick visual content for meetings.
---

# Frontend Slides — Zero-Dep HTML Presentations

## Quick Start

Generate a single `slides.html` file. Open in browser. Present.

## Slide Structure

Each presentation is a single HTML file with this structure:

```html
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>{{PRESENTATION_TITLE}}</title>
  <style>
    /* All styles inline — zero dependencies */
    {{STYLES}}
  </style>
</head>
<body>
  <div class="slide-deck">
    <div class="slide" id="slide-1">
      <!-- Slide content -->
    </div>
    <div class="slide" id="slide-2">
      <!-- Slide content -->
    </div>
    <!-- ... more slides -->
  </div>

  <div class="progress-bar"><div class="progress-fill"></div></div>
  <div class="slide-counter"></div>

  <script>
    /* All JS inline — keyboard nav, touch, progress */
    {{SCRIPT}}
  </script>
</body>
</html>
```

## Core CSS

```css
* { margin: 0; padding: 0; box-sizing: border-box; }

:root {
  --bg: #1a1a2e;
  --text: #eee;
  --accent: #e94560;
  --accent2: #0f3460;
  --font: 'system-ui', -apple-system, sans-serif;
  --mono: 'SF Mono', 'Fira Code', monospace;
}

html, body {
  height: 100%;
  overflow: hidden;
  font-family: var(--font);
  background: var(--bg);
  color: var(--text);
}

.slide-deck { height: 100%; position: relative; }

.slide {
  position: absolute;
  inset: 0;
  display: flex;
  flex-direction: column;
  justify-content: center;
  align-items: center;
  padding: 5vw;
  opacity: 0;
  transform: translateX(100%);
  transition: all 0.4s cubic-bezier(0.4, 0, 0.2, 1);
  text-align: center;
}

.slide.active {
  opacity: 1;
  transform: translateX(0);
}

.slide.prev {
  opacity: 0;
  transform: translateX(-100%);
}

/* Typography */
.slide h1 { font-size: clamp(2rem, 5vw, 4rem); margin-bottom: 1rem; }
.slide h2 { font-size: clamp(1.5rem, 3.5vw, 2.5rem); margin-bottom: 0.75rem; }
.slide p { font-size: clamp(1rem, 2vw, 1.5rem); line-height: 1.6; max-width: 40em; }
.slide ul, .slide ol { font-size: clamp(1rem, 1.8vw, 1.3rem); text-align: left; line-height: 1.8; }
.slide code { font-family: var(--mono); background: rgba(255,255,255,0.1); padding: 0.2em 0.4em; border-radius: 4px; }
.slide pre {
  background: rgba(0,0,0,0.3);
  padding: 1.5em;
  border-radius: 8px;
  overflow-x: auto;
  text-align: left;
  font-size: clamp(0.8rem, 1.4vw, 1.1rem);
  max-width: 90%;
}

/* Progress bar */
.progress-bar {
  position: fixed; bottom: 0; left: 0; right: 0;
  height: 3px; background: rgba(255,255,255,0.1);
}
.progress-fill {
  height: 100%; background: var(--accent);
  transition: width 0.3s ease;
}

/* Slide counter */
.slide-counter {
  position: fixed; bottom: 12px; right: 16px;
  font-size: 0.8rem; opacity: 0.5;
}

/* Speaker notes (hidden in presentation, visible in print) */
.notes { display: none; }

/* Print CSS */
@media print {
  .slide {
    position: relative !important;
    opacity: 1 !important;
    transform: none !important;
    page-break-after: always;
    min-height: 100vh;
  }
  .progress-bar, .slide-counter { display: none; }
  .notes { display: block; border-top: 1px solid #ccc; margin-top: 2em; font-size: 0.9em; color: #666; }
}

/* Responsive */
@media (max-width: 768px) {
  .slide { padding: 8vw 5vw; }
}
```

## Core JavaScript

```javascript
(function() {
  const slides = document.querySelectorAll('.slide');
  const progress = document.querySelector('.progress-fill');
  const counter = document.querySelector('.slide-counter');
  let current = 0;

  function show(index) {
    if (index < 0 || index >= slides.length) return;
    slides[current].classList.remove('active');
    slides[current].classList.add(index > current ? 'prev' : '');
    current = index;
    slides[current].classList.remove('prev');
    slides[current].classList.add('active');

    // Update progress
    progress.style.width = ((current + 1) / slides.length * 100) + '%';
    counter.textContent = (current + 1) + ' / ' + slides.length;

    // Update URL hash
    history.replaceState(null, '', '#slide-' + (current + 1));
  }

  // Keyboard navigation
  document.addEventListener('keydown', e => {
    if (e.key === 'ArrowRight' || e.key === ' ' || e.key === 'Enter') {
      e.preventDefault(); show(current + 1);
    }
    if (e.key === 'ArrowLeft' || e.key === 'Backspace') {
      e.preventDefault(); show(current - 1);
    }
    if (e.key === 'Home') { e.preventDefault(); show(0); }
    if (e.key === 'End') { e.preventDefault(); show(slides.length - 1); }
    // Number keys jump to slide
    if (e.key >= '1' && e.key <= '9') show(parseInt(e.key) - 1);
  });

  // Touch/swipe support
  let touchStart = 0;
  document.addEventListener('touchstart', e => touchStart = e.touches[0].clientX);
  document.addEventListener('touchend', e => {
    const diff = touchStart - e.changedTouches[0].clientX;
    if (Math.abs(diff) > 50) show(current + (diff > 0 ? 1 : -1));
  });

  // Click navigation (left half = back, right half = forward)
  document.addEventListener('click', e => {
    if (e.target.closest('a, button, code, pre')) return;
    show(current + (e.clientX < window.innerWidth / 2 ? -1 : 1));
  });

  // Hash navigation
  const hash = parseInt(location.hash.replace('#slide-', ''));
  show(isNaN(hash) ? 0 : hash - 1);
})();
```

## Slide Templates

### Title Slide
```html
<div class="slide" style="background: linear-gradient(135deg, var(--accent2), var(--bg));">
  <h1>Presentation Title</h1>
  <p style="opacity: 0.7;">Subtitle or tagline</p>
  <p style="margin-top: 2em; font-size: 0.9em; opacity: 0.5;">Author · Date</p>
</div>
```

### Content Slide
```html
<div class="slide">
  <h2>Slide Title</h2>
  <ul>
    <li>Point one with supporting detail</li>
    <li>Point two with <code>inline code</code></li>
    <li>Point three with emphasis</li>
  </ul>
  <div class="notes">Speaker notes go here — visible when printing.</div>
</div>
```

### Code Slide
```html
<div class="slide">
  <h2>Code Example</h2>
  <pre><code>function hello() {
  console.log("Hello, world!");
}</code></pre>
</div>
```

### Image Slide
```html
<div class="slide">
  <h2>Visual</h2>
  <img src="data:image/svg+xml,..." alt="Diagram" style="max-width: 80%; max-height: 60vh;">
</div>
```

### Two-Column Slide
```html
<div class="slide">
  <h2>Comparison</h2>
  <div style="display: grid; grid-template-columns: 1fr 1fr; gap: 2em; text-align: left; width: 100%;">
    <div>
      <h3 style="color: var(--accent);">Before</h3>
      <ul><li>Item</li></ul>
    </div>
    <div>
      <h3 style="color: #4CAF50;">After</h3>
      <ul><li>Item</li></ul>
    </div>
  </div>
</div>
```

### Closing Slide
```html
<div class="slide" style="background: linear-gradient(135deg, var(--bg), var(--accent2));">
  <h1>Thank You</h1>
  <p>Questions?</p>
  <p style="margin-top: 2em; font-size: 0.9em; opacity: 0.5;">contact@example.com</p>
</div>
```

## Color Themes

Override CSS variables for different themes:

```css
/* Dark (default) */
:root { --bg: #1a1a2e; --text: #eee; --accent: #e94560; --accent2: #0f3460; }

/* Light */
:root { --bg: #fafafa; --text: #222; --accent: #e94560; --accent2: #cce5ff; }

/* Corporate */
:root { --bg: #fff; --text: #333; --accent: #0066cc; --accent2: #e8f0fe; }

/* Neon */
:root { --bg: #0a0a0a; --text: #f0f0f0; --accent: #00ff88; --accent2: #1a0033; }
```

## Workflow

1. User describes the presentation topic and audience
2. Generate slide outline (8-15 slides typical)
3. Create the single HTML file with all slides
4. Write to `{topic-slug}-slides.html`
5. Instruct user to open in browser (`open slides.html` on macOS)

## Best Practices

- **Max 6 bullet points per slide** -- Less is more
- **One idea per slide** -- Don't overload
- **Use code slides for technical content** -- Pre-formatted blocks
- **Include speaker notes** -- Hidden in presentation, shown in print
- **Test print** -- Cmd+P should produce clean handouts
- **Keep total slides 8-20** -- Respect attention spans
