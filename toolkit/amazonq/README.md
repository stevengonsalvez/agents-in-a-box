# Amazon Q Developer rules (deployed to `.amazonq/rules/`)

Amazon Q Developer-specific rule store. `bootstrap.js` copies the rule files here into the target project's `.amazonq/rules/`, and `mcp.json` to `.amazonq/mcp.json` if MCP servers are configured. Cross-tool generic rules come from `../general-rules/`.
