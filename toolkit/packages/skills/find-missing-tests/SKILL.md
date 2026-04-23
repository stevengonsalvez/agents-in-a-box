---
name: find-missing-tests
description: Analyze codebase and identify missing test cases, then create GitHub issues for each gap
user-invocable: true
---

Analyze codebase and identify missing test cases, then create GitHub issues for each gap: $ARGUMENTS

Follow these steps:

<!-- recall:begin -->

## Step 0: Prior-art check (RECOMMENDED)

Before identifying test gaps, recall prior learnings from the global knowledge base so we don't re-learn or re-decide something already captured:

```bash
uv run "{{HOME_TOOL_DIR}}/skills/recall/scripts/recall.py" \
  "<QUERY>" \
  --limit 5 --format markdown
```

**Query construction for `/find-missing-tests`**: target module/package name + `"testing"` (e.g. `"payment webhook handler testing"`).

**What to do with results:**

- If a returned learning names a constraint, anti-pattern, or prior decision directly relevant to the task — surface it to the user BEFORE proceeding with this skill's main flow.
- If nothing relevant returns — proceed silently, no need to mention the check.
- Never block on recall failure. Empty output / non-zero exit is expected when the KB is absent or the subprocess errors — treat it as "no prior art found", not as an error.

<!-- recall:end -->

1. **Analyze the codebase**:
   - If $ARGUMENTS specifies files/directories, focus on those areas
   - Otherwise, scan the entire codebase for test coverage gaps
   - Look for functions, classes, and modules without corresponding tests
   - Identify edge cases and error conditions that aren't tested

2. **Review existing test structure**:
   - Understand the current testing framework and patterns
   - Identify test file naming conventions
   - Note any existing test utilities or helpers
   - Check for integration and end-to-end test coverage

3. **Identify specific missing test cases**:
   - **Unit tests**: Functions and methods without test coverage
   - **Integration tests**: Component interactions not tested
   - **Edge cases**: Boundary conditions, error handling, null/empty inputs
   - **Regression tests**: Previously fixed bugs without test coverage
   - **Performance tests**: Critical paths without performance validation

4. **Create detailed GitHub issues** for each missing test with:
   - Clear title: `[TEST] Add {type} tests for {component/function}`
   - Specific test cases to implement
   - Expected test file location following project conventions
   - Acceptance criteria defining what constitutes complete coverage
   - Priority level based on code criticality

5. **Generate GitHub issues**:
   - Use `gh issue create` for each identified test gap
   - Add appropriate labels: `testing`, `bug`, `enhancement`
   - Include code snippets showing what needs testing
   - Reference related code files and line numbers

6. **Create a summary report**:
   - Save findings as `test-coverage-analysis.md`
   - Include overall coverage assessment
   - Prioritize issues by criticality and risk

Remember: Be specific about what should be tested - don't create vague "add more tests" issues.
