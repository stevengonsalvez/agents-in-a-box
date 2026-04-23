---
name: brainstorm
description: Develop a thorough, step-by-step specification for a given idea
user-invocable: true
---

Develop a thorough, step-by-step specification for the provided idea: $ARGUMENTS

Follow these steps:

<!-- recall:begin -->

## Step 0: Prior-art check (MANDATORY)

Before brainstorming, recall prior learnings from the global knowledge base so we don't re-learn or re-decide something already captured:

```bash
uv run "{{HOME_TOOL_DIR}}/skills/recall/scripts/recall.py" \
  "<QUERY>" \
  --limit 5 --format markdown
```

**Query construction for `/brainstorm`**: the topic or problem being brainstormed + any constraint hints from the user (e.g. `"caching strategy mobile offline"`).

**What to do with results:**

- If a returned learning names a constraint, anti-pattern, or prior decision directly relevant to the task — surface it to the user BEFORE proceeding with this skill's main flow.
- If nothing relevant returns — proceed silently, no need to mention the check.
- Never block on recall failure. Empty output / non-zero exit is expected when the KB is absent or the subprocess errors — treat it as "no prior art found", not as an error.

<!-- recall:end -->

1. **Ask clarifying questions** one at a time to understand the requirements:
   - What is the core problem this solves?
   - Who are the target users?
   - What are the key features and functionality?
   - What are the technical constraints or preferences?
   - What is the expected timeline and scope?

2. **Build iteratively** on each answer to create a comprehensive spec that includes:
   - Problem statement and goals
   - User personas and use cases
   - Functional requirements
   - Technical architecture overview
   - Data models and API design
   - UI/UX considerations
   - Testing strategy
   - Deployment requirements

3. **Save the specification** as `spec.md` with proper markdown formatting

4. **Offer GitHub repository creation**:
   - Ask if they want to create a new GitHub repository
   - If yes, use `gh repo create` to create the repo
   - Commit the `spec.md` file
   - Push to the newly created repository

Remember: Ask only one question at a time and build on previous answers iteratively.
