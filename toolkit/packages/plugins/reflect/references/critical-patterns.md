# Critical Patterns - Required Reading

This document contains patterns that ALL agents should know before starting work. These are automatically included in planning context.

---

## How to Use This Document

1. **Before Planning**: Review relevant sections for your task
2. **During Work**: Check if any pattern applies to your situation
3. **After Solving**: Add new critical patterns here if widely applicable

---

## General Patterns

### Always Check Existing Implementations First

Before implementing something new:
1. Search for similar implementations in the codebase
2. Check if there's a utility function that does what you need
3. Look for established patterns to follow

**Why**: Consistency matters. Reinventing reduces maintainability.

### Fail Fast, Fail Clearly

- Validate inputs at system boundaries
- Return early on error conditions
- Include context in error messages (what failed, why, what to do)

### The Rule of Three

- First time: Just do it
- Second time: Note the duplication
- Third time: Abstract it

**Don't abstract prematurely. Wait for the pattern to prove itself.**

---

## Language-Specific Patterns

### Rust

**Async/Await**
- Never call `block_on()` inside an async context
- Use `tokio::task::spawn_blocking` for CPU-bound work in async code
- Prefer `tokio::select!` over manual future management

**Error Handling**
- Use `thiserror` for library errors, `anyhow` for application errors
- Always provide context with `.context("what was happening")`
- Don't use `.unwrap()` in production code

### TypeScript/JavaScript

**Async Patterns**
- Always handle promise rejections
- Use `Promise.allSettled` when failures shouldn't stop other operations
- Avoid mixing callbacks and promises

**Type Safety**
- Prefer `unknown` over `any`
- Use discriminated unions for state machines
- Define API responses as types, not just `any`

### Python

**Async**
- Don't mix sync and async without `asyncio.to_thread()`
- Use `asyncio.gather` with `return_exceptions=True` for parallel ops
- Context managers (`async with`) for resource cleanup

---

## Database Patterns

### Migrations

- Always make migrations reversible
- Never delete data in a migration without backup
- Test migrations on a copy of production data first
- Add indexes CONCURRENTLY to avoid locking

### Queries

- Never use `SELECT *` in production queries
- Always add `LIMIT` to potentially large result sets
- Use parameterized queries (never string concatenation)
- Check query plans for N+1 issues

---

## API Patterns

### Error Responses

Always include:
- HTTP status code (appropriate, not just 500)
- Error code (machine-readable)
- Message (human-readable)
- Request ID (for debugging)

### Pagination

- Use cursor-based pagination for large datasets
- Always return total count (or indicate if more results exist)
- Include links to next/previous pages

---

## Testing Patterns

### What to Test

- Test behavior, not implementation
- Focus on edge cases and error paths
- Test the public API, not internal methods

### Test Structure

```
Arrange: Set up test data
Act: Perform the action
Assert: Verify the result
```

### Flaky Tests

When a test is flaky:
1. Don't just re-run and hope - investigate
2. Look for timing dependencies
3. Check for shared state between tests
4. Consider if the test is testing the right thing

---

## Security Patterns

### Never Trust User Input

- Validate and sanitize all external input
- Use parameterized queries (SQL injection)
- Escape output appropriately (XSS)
- Verify file uploads (type, size, content)

### Authentication

- Never store passwords in plain text
- Use constant-time comparison for tokens
- Implement rate limiting on auth endpoints
- Log authentication failures (but not passwords)

### Secrets

- Never commit secrets to version control
- Use environment variables or secret managers
- Rotate compromised secrets immediately
- Audit secret access

---

## Deployment Patterns

### Before Deploying

- [ ] All tests pass
- [ ] No console.log/print statements left
- [ ] Database migrations are ready
- [ ] Feature flags in place for risky changes
- [ ] Rollback plan documented

### Zero-Downtime Deployments

1. Deploy new version alongside old
2. Gradually shift traffic
3. Monitor for errors
4. Complete cutover or rollback

---

## Add Your Patterns

When you discover a pattern that:
- Applies across multiple situations
- Prevents common mistakes
- Is not obvious to newcomers

Add it here with:
1. Clear title
2. Brief explanation
3. Why it matters
4. Example if helpful
