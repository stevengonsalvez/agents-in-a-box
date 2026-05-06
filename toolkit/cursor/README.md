# cursor rules (deployed to `.cursor/rules/`)

Cursor-specific rule files. `bootstrap.js` reads `cursor-rulestore-rule.md` here and writes it into the target project's `.cursor/rules/` when the user picks `cursor` as their tool. This is the single rule cursor needs to know about — the rest comes from `../general-rules/`.
