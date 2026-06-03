// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// https://astro.build/config
export default defineConfig({
  site: 'https://stevengonsalvez.github.io',
  base: '/agents-in-a-box',
  trailingSlash: 'never',
  integrations: [
    starlight({
      title: 'agents-in-a-box',
      description: 'Terminal-native ecosystem for managing AI coding agents.',
      favicon: '/favicon.svg',
      social: [
        {
          icon: 'github',
          label: 'GitHub',
          href: 'https://github.com/stevengonsalvez/agents-in-a-box',
        },
      ],
      customCss: ['./src/styles/tokens.css', './src/styles/crt.css'],
      editLink: {
        baseUrl: 'https://github.com/stevengonsalvez/agents-in-a-box/edit/main/',
      },
      lastUpdated: true,
      pagination: true,
      head: [
        { tag: 'meta', attrs: { name: 'theme-color', content: '#0F0F18' } },
        { tag: 'link', attrs: { rel: 'preconnect', href: 'https://fonts.googleapis.com' } },
        { tag: 'link', attrs: { rel: 'preconnect', href: 'https://fonts.gstatic.com', crossorigin: '' } },
        {
          tag: 'link',
          attrs: {
            rel: 'stylesheet',
            href: 'https://fonts.googleapis.com/css2?family=IBM+Plex+Mono:wght@400;500;700&family=JetBrains+Mono:wght@400;500;700&display=swap',
          },
        },
      ],
      sidebar: [
        {
          label: 'Start here',
          items: [
            { label: 'What is ainb?', slug: 'product/what-is-ainb' },
            { label: 'Value proposition', slug: 'product/value' },
            { label: 'Architecture', slug: 'product/architecture' },
          ],
        },
        {
          label: 'TUI',
          items: [
            { label: 'Overview', slug: 'tui/overview' },
            { label: 'Install', slug: 'tui/install' },
            { label: 'Quickstart', slug: 'tui/quickstart' },
            { label: 'CLI reference', slug: 'tui/cli' },
            { label: 'Keyboard shortcuts', slug: 'tui/keyboard-shortcuts' },
            { label: 'Architecture', slug: 'tui/architecture' },
            { label: 'FAQ', slug: 'tui/faq' },
          ],
        },
        {
          label: 'Toolkit',
          items: [
            { label: 'Overview', slug: 'toolkit/overview' },
            { label: 'Skills', slug: 'toolkit/skills' },
            { label: 'Agents', slug: 'toolkit/agents' },
            { label: 'Bootstrap engine', slug: 'toolkit/bootstrap' },
          ],
        },
        {
          label: 'Plugins',
          items: [
            { label: 'Disambiguation', slug: 'plugins/readme' },
            { label: 'Overview', slug: 'plugins/overview' },
            { label: 'User guide', slug: 'plugins/user-guide' },
            { label: 'Authoring guide', slug: 'plugins/authoring' },
            { label: 'Wire spec v2', slug: 'plugins/spec-v2' },
            { label: 'Changelog', slug: 'plugins/changelog' },
          ],
        },
        {
          label: 'Knowledge',
          items: [
            { label: 'How reflection works', slug: 'knowledge/overview' },
            { label: 'Hooks & platform (Claude + Codex)', slug: 'knowledge/hooks-and-platform' },
            { label: 'reflect CLI', slug: 'knowledge/reflect-cli' },
          ],
        },
        {
          label: 'Contributing',
          items: [
            { label: 'Building', slug: 'contributing/building' },
            { label: 'CI / CD', slug: 'contributing/ci-cd' },
            { label: 'Release process', slug: 'contributing/release-process' },
          ],
        },
        {
          label: 'Hangar',
          items: [
            { label: 'Architecture & features', slug: 'hangar/architecture' },
          ],
        },
        {
          label: 'Reference',
          items: [
            { label: 'Architecture deep-dive', slug: 'reference/architecture' },
            { label: 'Glossary', slug: 'reference/glossary' },
          ],
        },
      ],
    }),
  ],
});
