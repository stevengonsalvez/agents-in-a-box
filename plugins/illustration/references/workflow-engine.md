# Illustration workflow engine (shared)

Every mascot sub-skill in this plugin (`illustration:sporthead-alex`, `illustration:popa`, …) shares this engine. The sub-skill supplies the *visual DNA*, *IP anchors*, and *reference images*; this file supplies the *process*. Read this, then read your mascot's own `references/`.

## The five steps

### 1. Digest the source

Read whatever the user gives: article, README, codebase, deck, idea, or rant. Extract:

- The core argument or system.
- Which parts carry the cognitive turns.
- What is image-worthy vs better left as text.

Do not illustrate evenly. Pick cognitive anchors: the core judgment, breakpoints, input/output loops, splits, before/after contrasts, common pitfalls, state changes, the one part everyone trips on. For brand/marketing work, the anchor is the single feeling or promise the piece must land.

### 2. Shot list first

When the user asks for strategy ("where do images go / how should we illustrate this"), output a shot list before generating. Per image:

- Placement / destination.
- Theme and core idea.
- Structure type and format preset (aspect) — see `composition-patterns.md`.
- What the mascot is doing.
- Suggested elements and labels.
- Optional: the mascot's signature gag/beat.

Default 4–8 for an article; 1 for a single social post; a series-of-3-to-5 for an architecture explainer.

### 3. Generate

When asked to generate, don't stop for confirmation. Generate each image separately via the image backend (nano-banana-pro / `image_gen`); never collage. Fill the mascot's `references/prompt-template.md` with the right aspect preset.

**Likeness rule (MANDATORY for every mascot):** pass the mascot's reference image(s) from its `assets/<mascot>-ref/` as inputs to the backend. Words alone drift the character off-model. The instruction is always "this is <Mascot>; draw THIS exact character, translated into <the mascot's style>; do not copy the reference's background."

Every prompt must carry the mascot's full set of identity anchors and its style DNA.

Invent a fresh metaphor/composition per image from the current source. Never reuse the bundled example compositions unless explicitly asked.

### 4. Check and iterate

Run the mascot's `references/qa-checklist.md`. Common regenerate/edit triggers across all mascots: the mascot is decoration not actor, an identity anchor is missing, the aspect drifted (verify dimensions, don't eyeball), labels exceed the cap, the frame is crowded, or it drifted off the mascot's style.

### 5. Save and deliver

Copy finals to `assets/<slug>-illustrations/`, numbered `01-topic-name.png`. Keep originals; never overwrite existing assets without being asked.

Delivery report: how many, what each is for, save paths, which are solid vs optional. Keep it short — let the images talk.

## Picking a mascot

If the user invokes the bare `illustration` skill without naming a mascot, ask which one (or infer from context: ShotClubhouse/sport/brand → `alex`; generic dev/system explainers with a soft funny tone → `popa`).
