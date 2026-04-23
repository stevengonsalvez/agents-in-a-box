---
name: plan-tdd
description: Create a test-driven development plan for the project
user-invocable: true
---

Create a test-driven development plan for the project: $ARGUMENTS

Follow these steps:

<!-- recall:begin -->

## Step 0: Prior-art check (MANDATORY)

Before writing the TDD plan, recall prior learnings from the global knowledge base so we don't re-learn or re-decide something already captured:

```bash
uv run "{{HOME_TOOL_DIR}}/skills/recall/scripts/recall.py" \
  "<QUERY>" \
  --limit 5 --format markdown
```

**Query construction for `/plan-tdd`**: task description + `"testing strategy"` + target module name (e.g. `"payment webhook testing strategy"`).

**What to do with results:**

- If a returned learning names a constraint, anti-pattern, or prior decision directly relevant to the task — surface it to the user BEFORE proceeding with this skill's main flow.
- If nothing relevant returns — proceed silently, no need to mention the check.
- Never block on recall failure. Empty output / non-zero exit is expected when the KB is absent or the subprocess errors — treat it as "no prior art found", not as an error.

<!-- recall:end -->

1. **Read the specification**:
   - Look for the spec file specified in $ARGUMENTS
   - If no file specified, look for `spec.md` or ask for the specification file path
   - Understand the project requirements, acceptance criteria, and expected behavior

2. **Draft a TDD-focused development blueprint**:
   - Break down the project into testable components and features
   - Identify the testing strategy: unit, integration, and end-to-end tests
   - Plan for incremental development with early and continuous testing
   - Ensure each step includes comprehensive test coverage
   - Design for testability from the beginning

3. **Create test-first development chunks**:
   - Review the breakdown and organize into TDD cycles
   - Ensure steps are small enough to implement safely with strong testing
   - Make sure steps provide meaningful progress while maintaining test coverage
   - Iterate until steps are appropriately sized for TDD methodology

4. **Generate TDD implementation prompts**:
   - Create detailed prompts that follow the TDD red-green-refactor cycle
   - For each step: write failing tests first, then implementation, then refactor
   - Prioritize best practices, incremental progress, and comprehensive testing
   - Ensure no big jumps in complexity between TDD cycles
   - Make sure each prompt builds on previous work with maintained test coverage
   - End with integration steps that wire everything together under test

5. **Structure the TDD workflow**:
   - Define the testing framework and tools to be used
   - Establish test file organization and naming conventions
   - Plan for test data management and mock strategies
   - Include performance and integration testing milestones

6. **Generate comprehensive documentation**:
   - Save the TDD plan as `tdd-development-plan.md`
   - Create `tdd-implementation-prompts.md` with step-by-step TDD prompts
   - Generate `testing-strategy.md` outlining the overall test approach
   - Include examples of expected test structure and patterns

Remember: Every feature should be developed test-first, with failing tests written before any implementation code.
