# Signal Detection Patterns

Comprehensive reference for detecting correction signals and learning opportunities in conversations.

## Confidence Levels

### HIGH Confidence (Explicit Corrections)

These patterns indicate the user is explicitly stating a rule or correction. Always apply these learnings.

#### Negative Directives

| Pattern | Type | Example |
|---------|------|---------|
| `never` | correction | "Never use var in TypeScript" |
| `don't` / `do not` | correction | "Don't commit directly to main" |
| `stop doing` | correction | "Stop doing manual deployments" |
| `wrong` | correction | "That's wrong, use snake_case" |
| `not like that` | correction | "Not like that, indent with tabs" |
| `incorrect` | correction | "That's incorrect syntax" |
| `should not` / `shouldn't` | prohibition | "You shouldn't use eval()" |
| `must not` / `mustn't` | prohibition | "Must not expose secrets" |

**Regex Pattern:**
```regex
\b(never|don't|do not|stop doing|wrong|not like that|incorrect|should not|shouldn't|must not|mustn't)\b
```

#### Positive Directives

| Pattern | Type | Example |
|---------|------|---------|
| `always` | requirement | "Always validate inputs" |
| `must` | requirement | "You must use parameterized queries" |
| `required` | requirement | "It's required to have tests" |
| `the rule is` | explicit_rule | "The rule is no console.log in prod" |
| `correct way` | explicit_rule | "The correct way is to use hooks" |

**Regex Pattern:**
```regex
\b(always|must|required|the rule is|correct way)\b
```

#### Frustration Markers

These indicate repeated corrections - highest priority learnings.

| Pattern | Type | Example |
|---------|------|---------|
| `I already told you` | frustration | "I already told you about this" |
| `again?` | frustration | "Wrong again?" |
| `not again` | frustration | "Oh not again" |
| `how many times` | frustration | "How many times do I need to say..." |

**Regex Pattern:**
```regex
(I already told you|again\?|not again|how many times)
```

#### Explicit Rules

| Pattern | Type | Example |
|---------|------|---------|
| `the rule is` | explicit_rule | "The rule is we use ESLint" |
| `you should know` | explicit_rule | "You should know we use Prettier" |
| `remember that` | explicit_rule | "Remember that we're on Node 20" |
| `don't forget` | explicit_rule | "Don't forget the error handling" |

**Regex Pattern:**
```regex
(the rule is|you should know|remember that|don't forget)
```

### MEDIUM Confidence (Approved Approaches)

These patterns indicate the user approved a specific approach. Apply with reasonable confidence.

#### Approval Markers

| Pattern | Type | Example |
|---------|------|---------|
| `perfect` | approval | "Perfect, that's what I wanted" |
| `exactly` | approval | "Exactly right" |
| `that's right` | approval | "That's right, keep it" |
| `yes, like that` | approval | "Yes, like that" |
| `correct` | approval | "Correct implementation" |

**Regex Pattern:**
```regex
\b(perfect|exactly|that's right|yes, like that|correct)\b
```

#### Positive Feedback

| Pattern | Type | Example |
|---------|------|---------|
| `good` | positive_feedback | "Good approach" |
| `great job` | positive_feedback | "Great job on the refactor" |
| `well done` | positive_feedback | "Well done with the tests" |
| `nice` | positive_feedback | "Nice solution" |
| `excellent` | positive_feedback | "Excellent work" |

**Regex Pattern:**
```regex
\b(good|great job|well done|nice|excellent)\b
```

#### Continuation Markers

| Pattern | Type | Example |
|---------|------|---------|
| `keep doing` | continuation | "Keep doing it this way" |
| `continue with` | continuation | "Continue with this approach" |
| `stick with` | continuation | "Stick with React hooks" |

**Regex Pattern:**
```regex
\b(keep doing|continue with|stick with)\b
```

### LOW Confidence (Observations)

These patterns suggest preferences but require validation before encoding as rules.

#### Suggestions

| Pattern | Type | Example |
|---------|------|---------|
| `maybe` | suggestion | "Maybe try TypeScript" |
| `perhaps` | suggestion | "Perhaps use a different library" |
| `might want to` | suggestion | "You might want to add caching" |
| `consider` | suggestion | "Consider using memoization" |

**Regex Pattern:**
```regex
\b(maybe|perhaps|might want to|consider)\b
```

#### Observations

| Pattern | Type | Example |
|---------|------|---------|
| `seems like` | observation | "Seems like this works" |
| `appears to` | observation | "Appears to be faster" |
| `looks like` | observation | "Looks like a good pattern" |

**Regex Pattern:**
```regex
\b(seems like|appears to|looks like)\b
```

---

# Knowledge Signal Patterns

Patterns for detecting solved problems, discovered insights, and decisions worth preserving.
Knowledge signals capture the *what was learned*, while behavioral signals capture *how to behave*.

## HIGH Confidence (Explicit Resolution)

The user explicitly states a root cause, fix, or decision. Always capture these.

### Root Cause Identified

| Pattern | Learning Type | Example |
|---------|--------------|---------|
| `root cause was` | bug-fix | "The root cause was a missing index on user_id" |
| `the issue was` | bug-fix | "The issue was that RLS policies blocked the insert" |
| `the problem was` | bug-fix | "The problem was a race condition in the webhook handler" |
| `the bug was` | bug-fix | "The bug was in the date parsing timezone handling" |
| `caused by` | bug-fix | "Caused by a stale cache entry after deploy" |
| `because of` | bug-fix | "Failed because of a circular dependency" |
| `the reason was` | bug-fix | "The reason was the env var wasn't set in staging" |

**Regex Pattern:**
```regex
\b(root cause was|the issue was|the problem was|the bug was|caused by|because of|the reason was)\b
```

### Fix Confirmed

| Pattern | Learning Type | Example |
|---------|--------------|---------|
| `fixed by` | bug-fix | "Fixed by adding CASCADE to the DROP FUNCTION" |
| `the fix was` | bug-fix | "The fix was to use pressSequentially instead of fill()" |
| `the solution was` | bug-fix | "The solution was to add a retry with backoff" |
| `resolved by` | bug-fix | "Resolved by upgrading the SDK to v3.2" |
| `solved by` | bug-fix | "Solved by wrapping in spawn_blocking" |
| `the workaround is` | bug-fix | "The workaround is to clear the CDN cache first" |
| `the trick is` | pattern | "The trick is to use uuid5 seeds, not auth IDs" |

**Regex Pattern:**
```regex
\b(fixed by|the fix was|the solution was|resolved by|solved by|the workaround is|the trick is)\b
```

### Explicit Decision

| Pattern | Learning Type | Example |
|---------|--------------|---------|
| `chose .+ over` | decision | "Chose Supabase over Firebase because of RLS" |
| `decided to` | decision | "Decided to use server-side rendering for SEO" |
| `went with` | decision | "Went with Playwright over Cypress for cross-browser" |
| `the tradeoff is` | decision | "The tradeoff is latency vs consistency" |
| `better approach is` | decision | "The better approach is event sourcing here" |
| `should have used` | anti-pattern | "Should have used a migration instead of raw SQL" |

**Regex Pattern:**
```regex
\b(chose .+ over|decided to|went with|the tradeoff is|better approach is|should have used)\b
```

### Breakthrough Moment

| Pattern | Learning Type | Example |
|---------|--------------|---------|
| `turns out` | bug-fix | "Turns out the error message was completely misleading" |
| `finally` (+ fix context) | bug-fix | "Finally got it working by disabling SSR" |
| `that worked` | bug-fix | "Adding the --force flag, that worked" |
| `it's fixed` | bug-fix | "It's fixed now, was a missing CORS header" |
| `figured out` | bug-fix | "Figured out why the tests were flaky" |
| `found it` | bug-fix | "Found it -- the env was loading .env.local over .env" |

**Regex Pattern:**
```regex
\b(turns out|finally.{0,30}(work|fix|got)|that worked|it's fixed|figured out|found it)\b
```

## MEDIUM Confidence (Implicit Knowledge)

These indicate knowledge worth capturing but require more context to confirm value.

### Debugging Effort (Time Investment = Value)

| Pattern | Learning Type | Example |
|---------|--------------|---------|
| `spent .+ (hour\|min\|day)` | bug-fix | "Spent 3 hours tracking down this memory leak" |
| `took .+ to (find\|fix\|debug)` | bug-fix | "Took 45 minutes to find the missing await" |
| `after trying` | bug-fix | "After trying 5 different approaches..." |
| `been debugging` | bug-fix | "Been debugging this for a while, the issue is..." |
| `wasted time on` | anti-pattern | "Wasted time on mocking -- should have used real DB" |

**Regex Pattern:**
```regex
(spent|took)\s+\d+\s*(hour|min|day|h|m)|(after trying|been debugging|wasted time on)
```

### Documentation vs Reality

| Pattern | Learning Type | Example |
|---------|--------------|---------|
| `the docs say .+ but` | bug-fix | "The docs say to use v2 but v3 is actually required" |
| `undocumented` | pattern | "There's an undocumented --force flag that fixes it" |
| `not mentioned in` | pattern | "Not mentioned in the docs but you need to restart" |
| `contrary to` | correction | "Contrary to the README, you need Node 20+" |
| `misleading error` | bug-fix | "The error says 'not found' but it's actually a permissions issue" |
| `the error message (is\|was) (wrong\|misleading\|confusing)` | bug-fix | "The error message was misleading" |

**Regex Pattern:**
```regex
\b(the docs say .+ but|undocumented|not mentioned in|contrary to|misleading error)\b|the error message (is|was) (wrong|misleading|confusing)
```

### Environment & Configuration Gotchas

| Pattern | Learning Type | Example |
|---------|--------------|---------|
| `make sure to (set\|install\|configure\|enable)` | pattern | "Make sure to set NEXT_PUBLIC_ prefix for client vars" |
| `requires .+ installed` | pattern | "Requires libvips installed for sharp to work" |
| `only works (with\|when\|if\|on)` | pattern | "Only works with Node 20+, breaks on 18" |
| `needs to be .+ first` | pattern | "The migration needs to be run first before seeding" |
| `won't work (unless\|without\|if)` | pattern | "Won't work without the --legacy-peer-deps flag" |
| `you have to .+ before` | pattern | "You have to enable the extension before creating the table" |

**Regex Pattern:**
```regex
\b(make sure to|requires .+ installed|only works (with|when|if|on)|needs to be .+ first|won't work (unless|without|if)|you have to .+ before)\b
```

### Performance Insights

| Pattern | Learning Type | Example |
|---------|--------------|---------|
| `was slow because` | pattern | "Was slow because of N+1 queries in the loop" |
| `performance (improved\|degraded)` | pattern | "Performance improved 10x after adding the index" |
| `bottleneck (was\|is)` | pattern | "The bottleneck was the unindexed JOIN" |
| `N\+1` | anti-pattern | "Classic N+1 -- fixed with eager loading" |
| `memory leak` | bug-fix | "Memory leak from unclosed database connections" |
| `timeout` (+ cause) | bug-fix | "Timeout because the query scans the full table" |

**Regex Pattern:**
```regex
\b(was slow because|performance (improved|degraded)|bottleneck (was|is)|N\+1|memory leak|timeout.{0,40}(because|caused|from|due))\b
```

### Integration & Compatibility

| Pattern | Learning Type | Example |
|---------|--------------|---------|
| `breaks when` | anti-pattern | "Breaks when you deploy to ARM architecture" |
| `incompatible with` | anti-pattern | "React 19 is incompatible with the old context API" |
| `doesn't work with` | anti-pattern | "sharp doesn't work with Alpine Linux by default" |
| `conflicts with` | anti-pattern | "The middleware conflicts with the auth provider" |
| `version .+ (required\|needed\|breaks)` | pattern | "Version 4.x breaks the plugin API" |

**Regex Pattern:**
```regex
\b(breaks when|incompatible with|doesn't work with|conflicts with|version .+ (required|needed|breaks))\b
```

### Failed Approaches (What NOT to Do)

| Pattern | Learning Type | Example |
|---------|--------------|---------|
| `tried .+ but` | anti-pattern | "Tried using WebSocket but it doesn't work behind ALB" |
| `doesn't work because` | anti-pattern | "Mocking doesn't work because the DB types diverge" |
| `dead end` | anti-pattern | "Using grpc-web was a dead end -- no streaming support" |
| `don't bother with` | anti-pattern | "Don't bother with the official SDK, use the REST API" |
| `waste of time` | anti-pattern | "Optimizing the ORM was a waste of time -- raw SQL was needed" |
| `wrong approach` | anti-pattern | "Caching at the application layer was the wrong approach" |

**Regex Pattern:**
```regex
\b(tried .+ but|doesn't work because|dead end|don't bother with|waste of time|wrong approach)\b
```

### Architecture & Design Insights

| Pattern | Learning Type | Example |
|---------|--------------|---------|
| `the right abstraction` | pattern | "The right abstraction here is a command bus" |
| `should be (separated\|decoupled\|extracted)` | pattern | "Auth should be separated from the business logic" |
| `scales (better\|worse) (with\|when)` | pattern | "This scales worse when you have > 10K records" |
| `the pattern (is\|for)` | pattern | "The pattern for this is CQRS with event sourcing" |
| `lesson learned` | pattern | "Lesson learned: always use idempotency keys for payments" |

**Regex Pattern:**
```regex
\b(the right abstraction|should be (separated|decoupled|extracted)|scales (better|worse)|the pattern (is|for)|lesson learned)\b
```

## LOW Confidence (Observations)

These suggest something was learned but need validation. Captured as LOW confidence, promoted via `/reflect:status` review.

### Implicit Success

| Pattern | Learning Type | Example |
|---------|--------------|---------|
| `seems to work` | pattern | "Adding the index seems to work" |
| `this approach works` | pattern | "This approach works for our scale" |
| `so far so good` | pattern | "Migrated to edge functions, so far so good" |
| `looks like .+ (fixed\|solved\|works)` | bug-fix | "Looks like the retry logic fixed the flakiness" |

**Regex Pattern:**
```regex
\b(seems to work|this approach works|so far so good|looks like .+ (fixed|solved|works))\b
```

### Library & Tool Discoveries

| Pattern | Learning Type | Example |
|---------|--------------|---------|
| `discovered` | pattern | "Discovered that vitest has built-in coverage" |
| `found (a\|this) (tool\|library\|package)` | pattern | "Found this library called zod that does runtime validation" |
| `switched (to\|from)` | decision | "Switched from jest to vitest, much faster" |
| `replaced .+ with` | decision | "Replaced axios with native fetch" |

**Regex Pattern:**
```regex
\b(discovered|found (a|this) (tool|library|package)|switched (to|from)|replaced .+ with)\b
```

### Security Discoveries

| Pattern | Learning Type | Example |
|---------|--------------|---------|
| `vulnerability` | bug-fix | "Found a vulnerability in the file upload handler" |
| `exposed` (+ sensitive context) | bug-fix | "API keys were exposed in the client bundle" |
| `bypass` (+ security context) | bug-fix | "Users could bypass RLS via the REST API" |
| `injection` | bug-fix | "SQL injection possible through the search param" |
| `escalation` | bug-fix | "Privilege escalation via the admin check" |

**Regex Pattern:**
```regex
\b(vulnerability|exposed.{0,20}(key|secret|token|credential)|bypass.{0,20}(auth|RLS|security|check)|injection|escalation)\b
```

## Structural Signals (Non-Phrase)

Not everything is said in words. These structural patterns in the conversation indicate knowledge:

### Error -> Investigation -> Fix Arc

When the conversation shows this sequence, the entire arc is a knowledge signal:
1. An error message appears (in code output or user paste)
2. Multiple investigation steps follow (file reads, greps, debug attempts)
3. A resolution is reached (edit applied, test passes)

**Detection:** Look for error patterns followed by resolution within the same conversation segment.

### Configuration Change That Resolved Something

When a config file edit (`.env`, `tsconfig.json`, `Cargo.toml`, `docker-compose.yml`) is made and followed by success indicators ("works now", "tests pass", test output shows green), that config knowledge is worth capturing.

### Multiple Approaches Tried

When the conversation shows 2+ distinct approaches to the same problem before settling on one, the winning approach AND the failed ones are knowledge signals. Failed approaches become anti-patterns; the winning approach becomes a pattern.

## Knowledge Signal False Positives

### Ignore These

| Pattern | Why it's a false positive |
|---------|--------------------------|
| `the issue is` in a bug report quote | User is quoting a ticket, not reporting their own finding |
| `fixed by` in a changelog/commit | Referencing existing fix, not a new discovery |
| `turns out` in hypothetical | "If it turns out to be X, then..." -- not confirmed |
| `tried .+ but` in planning | "We could try X but..." -- hasn't happened yet |
| `the problem is` about external blockers | "The problem is the API is down" -- not a learnable fix |
| `finally` without fix context | "Finally, let's move on" -- not a breakthrough |

### Context Checks

Before capturing a knowledge signal:
1. Is the speaker reporting their OWN finding, or quoting someone else?
2. Is this a confirmed outcome, or a hypothesis?
3. Is the fix actionable and reproducible?
4. Would someone searching for the error/symptom benefit from this?

## Combining Knowledge Signals

When multiple knowledge patterns match in the same conversation segment:
1. Merge into a single learning note (avoid duplicates)
2. Use the highest confidence pattern to set overall confidence
3. Include the full arc: symptom -> investigation -> root cause -> fix
4. Extract anti-patterns from failed approaches within the same arc

---

## Category Detection

### Code Style

Patterns indicating code style preferences:

```regex
\b(naming|convention|style|format|indent|case|camelCase|snake_case|PascalCase)\b
\b(variable|function|class|method|parameter)\s+name
\b(semicolon|quote|single quote|double quote|tabs|spaces)\b
```

### Architecture

Patterns indicating architectural decisions:

```regex
\b(pattern|architecture|design|structure|module|component|service)\b
\b(separation|coupling|cohesion|dependency|layer|abstraction)\b
\b(microservice|monolith|serverless|event-driven)\b
```

### Process

Patterns indicating workflow preferences:

```regex
\b(workflow|process|step|procedure|protocol|practice)\b
\b(commit|branch|merge|review|deploy|test|ci|cd)\b
\b(pr|pull request|code review|approval)\b
```

### Domain

Patterns indicating business logic:

```regex
\b(business|domain|logic|rule|requirement|constraint)\b
\b(customer|user|client|account|order|payment|invoice)\b
\b(validate|verify|check|ensure|confirm)\b
```

### Tools

Patterns indicating tool preferences:

```regex
\b(tool|cli|command|terminal|shell|editor|ide)\b
\b(git|npm|yarn|pnpm|docker|kubernetes)\b
\b(config|setting|environment|variable|env)\b
```

### Security

Patterns indicating security-related learnings:

```regex
\b(security|vulnerability|exploit|injection|xss|csrf|auth)\b
\b(password|secret|credential|token|key)\s+(expos|leak|hardcod)
\b(validation|sanitiz|escap|encrypt|hash)\b
\b(OWASP|CVE|RLS|row.level.security)\b
```

### New Skill

Patterns indicating skill-worthy discoveries:

```regex
\b(workaround|trick|hack|solution|fix|debug|resolve)\b
\b(error|bug|issue|problem)\s+(was|is|fixed|solved|resolved)\b
(took|spent)\s+\d+\s*(min|minute|hour|day)
\b(finally|after trying|turns out|the issue was)\b
```

## Edge Cases

### False Positives to Avoid

Some patterns may match but don't indicate learnings:

- Rhetorical questions: "Why would you never do X?" (not a directive)
- Hypotheticals: "If you never use Y, then..." (conditional)
- Quotes/references: "The docs say never..." (not user's directive)
- Negations: "It's not wrong to..." (opposite meaning)

### Context Sensitivity

Consider surrounding context:

- "That's not wrong" - NOT a correction
- "That's not right" - IS a correction
- "Never mind" - NOT a directive about future behavior
- "Always" in a loop - NOT a behavioral directive

## Combining Patterns

When multiple patterns match:

1. Use the highest confidence pattern
2. If equal confidence, use the most specific category
3. Prefer explicit rules over implied preferences
4. Prioritize frustration markers (indicate repeated issue)

## Implementation Notes

The `signal_detector.py` script implements these patterns. To update patterns:

1. Add pattern to appropriate `*_PATTERNS` list
2. Run `python signal_detector.py --test` to verify
3. Add test case for new pattern
4. Update this documentation
