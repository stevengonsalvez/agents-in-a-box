---
name: gh-issue
description: Analyze and fix the specified GitHub issue
user-invocable: true
---

Analyze and fix the specified GitHub issue: $ARGUMENTS

Follow these steps:

<!-- recall:begin -->

## Step 0: Prior-art check (RECOMMENDED)

Before fixing the issue, recall prior learnings from the global knowledge base so we don't re-learn or re-decide something already captured:

```bash
uv run "{{HOME_TOOL_DIR}}/skills/recall/scripts/recall.py" \
  "<QUERY>" \
  --limit 5 --format markdown
```

**Query construction for `/gh-issue`**: issue title + top labels + repo name (e.g. `"flaky signup test frontend auth"`).

**What to do with results:**

- If a returned learning names a constraint, anti-pattern, or prior decision directly relevant to the task — surface it to the user BEFORE proceeding with this skill's main flow.
- If nothing relevant returns — proceed silently, no need to mention the check.
- Never block on recall failure. Empty output / non-zero exit is expected when the KB is absent or the subprocess errors — treat it as "no prior art found", not as an error.

<!-- recall:end -->

1. **Get issue details**:
   - Use `gh issue view $ARGUMENTS` to get the complete issue information
   - If $ARGUMENTS is not provided, list issues with `gh issue list` and ask which to work on
   - Understand the problem description, expected behavior, and acceptance criteria

2. **Understand the problem**:
   - Read through the issue description and any comments carefully
   - Identify the root cause and scope of the issue
   - Note any reproduction steps or examples provided

3. **Search the codebase**:
   - Use appropriate search tools to find relevant files and code sections
   - Understand the current implementation and identify what needs to change
   - Look for related code that might be affected by the fix

4. **Plan and comment your approach**:
   - Post a detailed implementation plan as a comment on the issue
   - Use `gh issue comment $ARGUMENTS --body "Implementation plan: ..."`
   - Include what files will be changed and the general approach

5. **Create a development branch**:
   - Create a new branch: `git checkout -b fix/issue-$ARGUMENTS-{description}`
   - Ensure you're working from the latest main branch

6. **Implement the fix**:
   - Make the necessary changes to fix the issue
   - Follow existing code patterns and conventions
   - Include comprehensive tests to verify the fix
   - Add debug logging where helpful

7. **Verify the solution**:
   - Run existing tests to ensure nothing breaks: `npm test` or equivalent
   - Run linting and type checking: `npm run lint`, `npm run typecheck`
   - Test your specific fix thoroughly
   - Ensure all tests pass

8. **Create a pull request**:
   - Commit changes with descriptive message referencing the issue
   - Push the branch to GitHub
   - Use `gh pr create --title "Fix #$ARGUMENTS: {description}" --body "Closes #$ARGUMENTS"`
   - Include details about the fix and testing performed

9. **Keep the issue open** until the pull request is merged

Remember: Use the GitHub CLI (`gh`) for all GitHub-related tasks and reference the issue number in commits and PR.
