# clawdhub

ClawdHub-ready skill packages. Each subdirectory here is a published (or publishable) skill that ships through the ClawdHub registry.

`bootstrap.js` reads this dir when the user picks `clawdhub` as their tool — the user selects which skills they want, and `bootstrap.js` copies the chosen skills into the target workspace's `skills/` directory.

| Skill | Purpose |
|-------|---------|
| `reflect/` | The reflect plugin packaged for ClawdHub distribution (mirror of `../packages/plugins/reflect/`) |

## Adding a skill

1. Create `clawdhub/<name>/` with the standard structure: `SKILL.md`, `README.md`, `skill.json`, `data/`.
2. Manually curate — only skills intended for public distribution belong here.
3. Test with `node bootstrap.js` → pick `clawdhub` → confirm your skill appears in the picker.

To publish: `clawdhub publish <target-dir>/<skill-name>` after copying.
