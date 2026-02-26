# Claude Code Troubleshooting

Common issues and solutions for Claude Code CLI.

---

## Workspace Trust Prompt Keeps Appearing

**Symptom**: Every session asks "Is this a project you created or one you trust?" even with `--dangerously-skip-permissions`.

**Root Cause**: Known bug in Claude Code 2.1.x where the trust dialog doesn't respect CLI flags or persisted settings properly.

**Solution**: Add `permissionMode: bypassPermissions` to `~/.claude/settings.json`:

```json
{
  "permissionMode": "bypassPermissions"
}
```

**Alternative workarounds** (less reliable):

1. **Pre-trust directories in `~/.claude/claude.json`**:
   ```json
   {
     "projects": {
       "/Users/you/projects": {
         "hasTrustDialogAccepted": true,
         "hasTrustDialogHooksAccepted": true
       }
     }
   }
   ```
   Note: Requires exact path match, no glob support.

2. **Create `.claude/` in parent directory** for worktree inheritance:
   ```bash
   mkdir -p /path/to/worktrees/.claude
   ```

3. **Use `-p` (pipe mode)** for headless/automated runs - skips trust prompt.

**Related Issues**:
- [#12261](https://github.com/anthropics/claude-code/issues/12261) - `--dangerously-skip-permissions` not working
- [#12737](https://github.com/anthropics/claude-code/issues/12737) - Feature request for `trustedDirectories`
- [#20629](https://github.com/anthropics/claude-code/issues/20629) - Trusted folder allowlist request

---

## Subagents Not Inheriting Permissions

**Symptom**: Permission prompts appear during Plan mode or subagent execution despite `--dangerously-skip-permissions`.

**Root Cause**: Subagents don't properly inherit parent process permission settings.

**Solution**: Same as above - use `permissionMode: bypassPermissions` in settings.json.

---

## Agent Teams Feature Not Available

**Symptom**: TeamCreate, SendMessage, and related tools not recognized.

**Solution**: Enable the experimental feature:

```json
{
  "env": {
    "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS": "1"
  }
}
```

Or set in shell: `export CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`

---

## Session Hooks Not Firing

**Symptom**: SessionStart, Stop, or other hooks don't execute.

**Checklist**:
1. Verify hook scripts are executable: `chmod +x ~/.claude/hooks/*.py`
2. Check hook command syntax in settings.json
3. Test hook manually: `uv run ~/.claude/hooks/session_start.py --git-status`
4. Check for Python/uv errors in hook output

---

## MCP Servers Not Connecting

**Symptom**: MCP tools unavailable or connection errors.

**Diagnosis**:
```bash
claude mcp list
claude --debug mcp
```

**Common fixes**:
1. Restart Claude Code after MCP config changes
2. Check server process is running
3. Verify socket/port availability

---

## Context Compaction Issues

**Symptom**: Session compacts too aggressively or loses important context.

**Solution**: Adjust the compaction threshold:

```json
{
  "env": {
    "CLAUDE_AUTOCOMPACT_PCT_OVERRIDE": "80"
  }
}
```

Higher values = more aggressive compaction. Default is ~70.

---

## Skills/Commands Not Found

**Symptom**: `/skill-name` returns "skill not found".

**Checklist**:
1. Verify skill exists: `ls ~/.claude/skills/skill-name/SKILL.md`
2. Check SKILL.md has valid YAML frontmatter with `name:` field
3. Re-run deployment: `node toolkit/create-rule.js --tool=claude-code-4.5`

---

## Further Resources

- [Claude Code Docs](https://code.claude.com/docs/en/)
- [GitHub Issues](https://github.com/anthropics/claude-code/issues)
- [Security Guide](https://code.claude.com/docs/en/security)
