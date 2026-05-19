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
      pattern: ['**/*.{md,mdx}', '!assets/**'],
    }),
    schema: docsSchema(),
  }),
};
