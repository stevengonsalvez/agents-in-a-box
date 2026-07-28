import { defineCollection } from 'astro:content';
import { glob } from 'astro/loaders';
import { docsSchema } from '@astrojs/starlight/schema';

// The repo's docs/ tree is the source of truth. We point the Starlight content
// collection at it so a single .md file lives in one place and is editable
// from the repo root without copying into the site source tree.
export const collections = {
  docs: defineCollection({
    loader: glob({
      base: '../../docs',
      pattern: [
        '**/*.{md,mdx}',
        '!assets/**',
        // Published docs are human-authored and review-gated. Automation may
        // only write under these excluded directories: explorations/ (research
        // notes, gap matrices, progress ledgers), hangar/ (internal build
        // artifacts, goals, specs, proofs), and solutions/ (reflect-generated
        // machine-written knowledge notes). None of these carry Starlight
        // frontmatter, so excluding the whole directory (rather than naming
        // files one at a time as they're added) keeps the site build from
        // breaking every time automation drops a new file in one of them.
        '!explorations/**',
        '!solutions/**',
        // hangar/architecture.md is the one exception: a real, human-authored
        // site page (see astro.config.mjs sidebar entry for slug
        // 'hangar/architecture'). A plain '!hangar/**' plus a later
        // re-include pattern does NOT work here: tinyglobby/fdir prunes
        // excluded directories from traversal entirely, so nothing under
        // hangar/ is even visited once '!hangar/**' is applied, and a later
        // positive pattern can't un-prune it. The extglob form below excludes
        // every hangar/ entry except architecture.md while still letting the
        // walker descend into hangar/ to find it.
        '!hangar/!(architecture.md)',
      ],
    }),
    schema: docsSchema(),
  }),
};
