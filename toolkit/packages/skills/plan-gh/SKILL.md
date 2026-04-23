---
name: plan-gh
description: Create a detailed development plan and corresponding GitHub issues
user-invocable: true
---

Create a detailed development plan and corresponding GitHub issues for the project: $ARGUMENTS

Follow these steps:

<!-- recall:begin -->

## Step 0: Prior-art check (MANDATORY)

Before generating issues, recall prior learnings from the global knowledge base so we don't re-learn or re-decide something already captured:

```bash
uv run "{{HOME_TOOL_DIR}}/skills/recall/scripts/recall.py" \
  "<QUERY>" \
  --limit 5 --format markdown
```

**Query construction for `/plan-gh`**: task scope + `"github issue"` + repo/project name (e.g. `"dashboard rewrite github issue frontend"`).

**What to do with results:**

- If a returned learning names a constraint, anti-pattern, or prior decision directly relevant to the task — surface it to the user BEFORE proceeding with this skill's main flow.
- If nothing relevant returns — proceed silently, no need to mention the check.
- Never block on recall failure. Empty output / non-zero exit is expected when the KB is absent or the subprocess errors — treat it as "no prior art found", not as an error.

<!-- recall:end -->

1. **Read the specification**:
   - Look for the spec file specified in $ARGUMENTS
   - If no file specified, look for `spec.md` or ask for the specification file path
   - Understand the project requirements, goals, and technical constraints

2. **Draft a comprehensive development blueprint**:
   - Break down the project into major phases and milestones
   - Identify dependencies between different components
   - Consider technical architecture and implementation approach
   - Plan for incremental, iterative development
   - Ensure each step builds safely on the previous step

3. **Create small, manageable development chunks**:
   - Review the initial breakdown and further decompose large tasks
   - Ensure steps are small enough to implement safely
   - Make sure steps are large enough to provide meaningful progress
   - Iterate until steps are appropriately sized for the project complexity

4. **Generate implementation prompts**:
   - Create detailed prompts for code-generation LLM for each step
   - Prioritize best practices and incremental progress
   - Ensure no big jumps in complexity between steps
   - Make sure each prompt builds on previous work
   - End with integration steps to wire everything together
   - Avoid creating orphaned or hanging code

5. **Create GitHub issues for each step**:
   - Use `gh issue create` for each development step
   - Apply appropriate labels: `epic`, `feature`, `task`
   - Include detailed acceptance criteria and implementation notes
   - Set up dependencies between issues using GitHub's linking
   - Assign priority levels and effort estimates

6. **Generate comprehensive documentation**:
   - Save the development plan as `development-plan.md`
   - Create `implementation-prompts.md` with LLM prompts for each step
   - Generate `project-roadmap.md` showing timeline and milestones
   - Include links to all created GitHub issues

Remember: Each step should be independently implementable while building toward the complete solution.
