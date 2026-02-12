# Research

You are tasked with conducting comprehensive research across multiple sources - codebase, web, and documentation - by spawning parallel sub-agents and synthesizing their findings.

## Initial Setup

When this command is invoked, respond with:
```
I'm ready to conduct comprehensive research. Please provide your research question or area of interest.

I can research:
- Codebase: Find implementations, patterns, and architecture
- Documentation: Discover existing docs and decisions
- Web: External resources, best practices, and solutions (if requested)

What would you like me to investigate?
```

Then wait for the user's research query.

## Research Process

### Step 1: Read Mentioned Files First

**CRITICAL**: If the user mentions specific files, read them FULLY first:
- Use the Read tool WITHOUT limit/offset parameters
- Read these files yourself in the main context before spawning any sub-tasks
- This ensures you have full context before decomposing the research

### Step 2: Analyze and Decompose

- Break down the query into composable research areas
- Think deeply about underlying patterns, connections, and architectural implications
- Identify specific components, patterns, or concepts to investigate
- Create a research plan using TodoWrite to track all subtasks
- Consider which directories, files, or architectural patterns are relevant

### Step 3: Spawn Parallel Research Tasks

Create multiple Task agents to research different aspects concurrently. Think deeply about the query to determine which types of research are needed.

**Research Types to Consider:**

**E. Learnings Research (ALWAYS do this FIRST):**

Before diving into codebase research, check if we've solved similar problems before.

```bash
# Search local docs/solutions/ for relevant past learnings
~/.claude/utils/search-learnings.sh "[query keywords]"

# Search with category filter
~/.claude/utils/search-learnings.sh -c build-errors "[error keywords]"

# Search with tag filter
~/.claude/utils/search-learnings.sh -t rust -t async "[query]"
```

**What to Search:**
- Error messages (exact or partial)
- Technology/framework names
- Problem descriptions
- Symptom keywords

**What to Extract:**
- `key_insight`: THE ONE THING that fixes the problem
- `root_cause`: Understanding why it happened
- `solution`: Steps to resolve
- Related learnings for broader context

**Critical Patterns:**
Always check `docs/solutions/patterns/critical-patterns.md` for patterns that apply to the current task. These are "Required Reading" that prevent common mistakes.

```
Task: "Search past learnings for [topic]"
- Run search-learnings.sh with relevant keywords
- Search by symptoms, tags, category
- Return key_insight field prominently
- Local results take priority over global
- Check critical-patterns.md for applicable patterns
```

**If Global Learnings Available:**
When a `global-learnings` repository is cloned (usually at `~/.claude/global-learnings/`):
```bash
# Search global GraphRAG index
learnings search "[query]"

# Get critical patterns for language/domain
learnings critical-patterns --language rust --domain backend
```

---

**A. Codebase Research (always do this):**
```
Task: "Find all files related to [topic]"
- Search for relevant source files, configs, tests
- Identify main implementation files
- Find usage examples and patterns
- Return specific file:line references

Task: "Analyze how [system/feature] works"
- Understand current implementation
- Trace data flow and dependencies
- Identify conventions and patterns
- Return detailed explanations with code references

Task: "Find similar implementations of [pattern]"
- Look for existing examples to model after
- Identify reusable components
- Find test patterns to follow
```

**B. Documentation Research (if relevant):**
```
Task: "Find existing documentation about [topic]"
- Search README files, docs directories
- Look for architecture decision records (ADRs)
- Find API documentation
- Check inline code comments for important notes

Task: "Extract insights from documentation"
- Synthesize key decisions and rationale
- Identify constraints and requirements
- Find historical context
```

**C. Web Research (if explicitly requested or needed):**
```
Task: "Research best practices for [technology/pattern]"
- MUST use WebSearch tool explicitly (not internal knowledge)
- Find official documentation
- Discover community solutions
- Identify common pitfalls and solutions
- MUST return specific URLs with findings
- Save all search results to /tmp/web-research-results-[timestamp].txt
- Include GitHub/GitLab/Bitbucket URLs found in results or citations

Task: "Find external resources about [topic]"
- MUST use WebSearch tool explicitly
- Look for tutorials, guides, examples
- Find relevant Stack Overflow discussions
- Discover blog posts or articles
- MUST include links for reference
- Save results to file: /tmp/web-research-results-[timestamp].txt
- Note any repository URLs mentioned in sources
```

**CRITICAL for Web Research Tasks**:
- Always use the WebSearch tool (DO NOT rely on internal knowledge)
- Save complete search results to `/tmp/web-research-results-$(date +%s).txt`
- Save agent response with URLs to `/tmp/agent-outputs-$(date +%s)-$$.txt`
- Include ALL URLs found (especially GitHub, GitLab, Bitbucket)
- Include repository URLs from citations and references
- Return the file path with search results for URL detection

**D. Test and Quality Research:**
```
Task: "Analyze test coverage for [component]"
- Find existing tests
- Identify testing patterns
- Check for missing test cases
- Return test file locations
```

**Spawning Strategy:**
- Run 3-6 focused tasks in parallel for efficiency
- Each task should have a clear, specific goal
- Provide enough context for agents to be effective
- Request concrete outputs (file paths, code snippets, URLs)

### Step 3.5: External Repository Discovery Follow-up

⚠️ **CRITICAL: YOU MUST EXECUTE THE BASH SCRIPT BELOW** ⚠️

**AUTOMATIC DETECTION** (runs if web research was performed):

After web research completes, **EXECUTE this bash script** to scan ALL web research results for external repository URLs:

```bash
# Detect repository URLs from all web research results
REPO_URLS=""
find /tmp -name "web-research-results-*.txt" -mmin -60 2>/dev/null | while IFS= read -r file; do
    URLS=$(bash ~/.claude/utils/detect-repo-urls.sh "$file")
    if [ -n "$URLS" ]; then
        REPO_URLS+="${URLS}"$'\n'
    fi
done

# Also scan agent outputs for any repository URLs they mentioned
# (agents may include GitHub URLs in their responses even without WebSearch)
# Scan all recent agent output files (timestamped to avoid session collisions)
find /tmp -name "agent-outputs-*-$$.txt" -mmin -60 2>/dev/null | while IFS= read -r file; do
    AGENT_URLS=$(bash ~/.claude/utils/detect-repo-urls.sh "$file" 2>/dev/null || echo "")
    if [ -n "$AGENT_URLS" ]; then
        REPO_URLS+="${AGENT_URLS}"$'\n'
    fi
done

# Deduplicate and display
REPO_URLS=$(echo "$REPO_URLS" | sort -u | grep -v '^$')

if [ -n "$REPO_URLS" ]; then
    echo "📦 Detected external repositories from web research:"
    echo "$REPO_URLS"

    # Save to file for Step 3.6 with unique identifier (timestamp + PID)
    DETECTED_REPOS_FILE="/tmp/detected-repos-$(date +%s)-$$.txt"
    echo "$REPO_URLS" > "$DETECTED_REPOS_FILE"
    echo "  Saved to: $DETECTED_REPOS_FILE"
fi
```

**Detection Sources**:
1. **WebSearch results** saved by Step 3C agents (primary source)
2. **Agent output citations** - URLs mentioned in agent responses
3. **Search result references** - Links in documentation/articles found

**Supported Platforms:**
- GitHub: `https://github.com/owner/repo`
- GitLab: `https://gitlab.com/owner/repo`
- Bitbucket: `https://bitbucket.org/owner/repo`

**What Happens Next:**
- Repository URLs are collected for analysis in Step 3.6
- Shallow clones (`--depth 1`) are prepared for efficient analysis
- Global cache checked at `~/.claude/research-cache/`
- Parallel analysis will be triggered automatically

**Note**: This step is informational only. The actual cloning and analysis happens in Step 3.6 (below Step 4).

### Step 4: Wait and Synthesize

- **IMPORTANT**: Wait for ALL sub-agent tasks to complete
- **SAVE AGENT OUTPUTS**: Save all agent responses to `/tmp/agent-outputs-$(date +%s)-$$.txt` for repository URL detection in Step 3.5
- Compile all sub-agent results
- Prioritize live codebase findings as primary source of truth
- Connect findings across different components
- Include specific file paths and line numbers for reference
- Highlight patterns, connections, and architectural decisions
- Answer the user's specific questions with concrete evidence

### Step 3.6: Parallel External Repository Analysis

⚠️ **CRITICAL: YOU MUST EXECUTE THE BASH SCRIPT BELOW IF REPOS WERE DETECTED** ⚠️

**AUTOMATIC EXECUTION** (runs if repositories detected in Step 3.5):

When external repositories are discovered, **EXECUTE this bash script** to analyze them in parallel:

```bash
# Check if repositories were detected in Step 3.5
# DETECTED_REPOS_FILE is set by Step 3.5, or find most recent if not set
if [ -z "$DETECTED_REPOS_FILE" ]; then
    DETECTED_REPOS_FILE=$(find /tmp -name "detected-repos-*-$$.txt" 2>/dev/null | sort -r | head -1)
fi

if [ -z "$DETECTED_REPOS_FILE" ] || [ ! -f "$DETECTED_REPOS_FILE" ]; then
    echo "ℹ No external repositories detected, skipping repository analysis"
    # Skip to Step 4
else
    echo "📦 Found detected repositories file: $DETECTED_REPOS_FILE"
    echo "🔬 Starting parallel repository analysis..."

    # Setup
    CLAUDE_HOME="${CLAUDE_HOME:-$HOME/.claude}"
    TEMP_CLONE_DIR=$(mktemp -d -t "external-repos-XXXXXXXX")
    CACHE_DIR="$CLAUDE_HOME/research-cache"
    mkdir -p "$CACHE_DIR"

    # Source utilities
    source "$CLAUDE_HOME/utils/cleanup-handler.sh"
    source "$CLAUDE_HOME/utils/repo-analysis-cache.sh"
    install_cleanup_traps
    register_cleanup_dir "$TEMP_CLONE_DIR"

    # Initialize cache
    bash "$CLAUDE_HOME/utils/repo-analysis-cache.sh" init

    # Build list of repos to clone (after cache checking)
    REPOS_TO_CLONE="$TEMP_CLONE_DIR/repos-to-clone.txt"
    REPOS_TO_ANALYZE="$TEMP_CLONE_DIR/repos-to-analyze.txt"
    CACHED_ANALYSES="$TEMP_CLONE_DIR/cached-analyses.txt"

    # Read detected repositories from Step 3.5
    while IFS= read -r repo_url; do
        # We need to clone to get commit hash first (shallow clone)
        # Then check if cached analysis exists for that commit

        # Add to clone list
        echo "$repo_url" >> "$REPOS_TO_CLONE"
    done < "$DETECTED_REPOS_FILE"

    # Clone repositories in parallel (shallow)
    bash "$CLAUDE_HOME/utils/parallel-repo-clone.sh" \
        "$REPOS_TO_CLONE" \
        "$TEMP_CLONE_DIR" \
        4  # max parallel clones

    # Check cache for each cloned repository
    for repo_dir in "$TEMP_CLONE_DIR"/*; do
        if [ -d "$repo_dir/.git" ]; then
            REPO_URL=$(git -C "$repo_dir" config --get remote.origin.url)
            COMMIT_HASH=$(git -C "$repo_dir" rev-parse HEAD)

            # Generate cache key
            CACHE_KEY=$(bash "$CLAUDE_HOME/utils/repo-analysis-cache.sh" key "$REPO_URL" "$COMMIT_HASH")

            # Check if cached analysis exists
            if bash "$CLAUDE_HOME/utils/repo-analysis-cache.sh" exists "$CACHE_KEY"; then
                # Use cached analysis
                CACHED_PATH=$(bash "$CLAUDE_HOME/utils/repo-analysis-cache.sh" get "$CACHE_KEY")
                echo "$CACHED_PATH|$REPO_URL|$COMMIT_HASH" >> "$CACHED_ANALYSES"
                echo "✓ Using cached analysis: $CACHE_KEY"
            else
                # Add to analysis queue
                echo "$repo_dir|$REPO_URL" >> "$REPOS_TO_ANALYZE"
            fi
        fi
    done

    # Analyze repositories not in cache
    if [ -f "$REPOS_TO_ANALYZE" ]; then
        while IFS='|' read -r repo_path repo_url; do
            # Get commit hash for caching
            COMMIT_HASH=$(git -C "$repo_path" rev-parse HEAD)
            CACHE_KEY=$(bash "$CLAUDE_HOME/utils/repo-analysis-cache.sh" key "$repo_url" "$COMMIT_HASH")

            # Build analyzer prompt
            PROMPT=$(bash "$CLAUDE_HOME/utils/build-analyzer-prompt.sh" \
                "$repo_path" \
                "$repo_url" \
                "$RESEARCH_QUERY" \
                "Discovered during web research for: $RESEARCH_QUERY")

            # Launch focused-repository-analyzer agent
            # Note: This is pseudocode showing intent, actual implementation spawns Task agents
            # Task:
            #   subagent_type: focused-repository-analyzer
            #   description: "Analyze $CACHE_KEY"
            #   prompt: "$PROMPT"
            #   run_in_background: false

            # After analysis completes, save to cache
            ANALYSIS_FILE="$CLAUDE_HOME/research-cache/$CACHE_KEY/analysis.md"
            if [ -f "$ANALYSIS_FILE" ]; then
                bash "$CLAUDE_HOME/utils/repo-analysis-cache.sh" save \
                    "$CACHE_KEY" \
                    "$ANALYSIS_FILE" \
                    "$repo_url" \
                    "$COMMIT_HASH" \
                    "$RESEARCH_QUERY" \
                    "Web research discovery"
            fi
        done < "$REPOS_TO_ANALYZE"
    fi
fi
```

**Performance Targets:**
- Clone all repos: 2-5 minutes (parallel)
- Analyze each repo: 25-35 minutes (parallel)
- Total: ~30-40 minutes for all repos combined

**Cache Strategy:**
- Save analysis to: `$CLAUDE_HOME/research-cache/<repo>-<commit>/analysis.md` (default: `~/.claude/research-cache/`)
- Check cache before cloning (7-day TTL)
- Reuse cached analysis if query similar (query hash matching)

**Output:**
Each analyzer agent produces:
- Focused analysis markdown in cache directory
- GitHub permalinks to relevant code
- Actionable recommendations
- Pattern extraction

**Integration:**
External repository findings will be integrated into the main research document in Step 5 under the "External Repository Analysis" section.

### Step 5: Generate Research Document

Create a document with the following structure:

```markdown
# Research: [User's Question/Topic]

**Date**: [Current date and time]
**Repository**: [Repository name]
**Branch**: [Current branch name]
**Commit**: [Current commit hash]
**Research Type**: [Codebase | Documentation | Web | Comprehensive]

## Research Question
[Original user query]

## Executive Summary
[2-3 sentence high-level answer to the question]

## Key Findings
- [Most important discovery]
- [Second key insight]
- [Third major finding]

## Prior Learnings (if found)

> **Note**: These learnings are from past solved problems. They may directly answer your question or provide crucial context.

### Relevant Past Solutions
| Learning | Key Insight | Confidence |
|----------|-------------|------------|
| [Title of learning] | [The one thing that fixes it] | high/medium/low |

### Critical Patterns Applied
- [Pattern from critical-patterns.md that applies]
- [Another relevant pattern]

### How Prior Learnings Inform This Research
[Explanation of how past solutions relate to current question]

---

## Detailed Findings

### Codebase Analysis
#### [Component/Area 1]
- Current implementation: [file.ext:line]
- How it works: [explanation]
- Dependencies: [what it relies on]
- Usage patterns: [how it's used elsewhere]

#### [Component/Area 2]
...

### Documentation Insights
- [Key documentation found]
- [Important decisions or constraints]
- [Historical context if relevant]

### External Research (if applicable)
- [Best practices from official docs] ([URL])
- [Community solutions] ([URL])
- [Relevant discussions] ([URL])

### External Repository Analysis (if repositories discovered)

For each analyzed external repository:

#### Repository: [owner/repo-name]
**URL**: [https://github.com/owner/repo](permalink)
**Commit**: `abc1234`
**Analysis**: [Link to cached analysis]($CLAUDE_HOME/research-cache/repo-abc1234/analysis.md)

**Key Findings:**
- **Implementation Pattern**: [How they implemented feature X] ([permalink to code])
- **Design Decision**: [Why they chose approach Y] ([permalink to code])
- **Code Example**: [Relevant code snippet with explanation] ([permalink to code])

**Relevance to Our Query:**
[How these findings answer our research question]

**Recommendations:**
- ✅ **Adopt**: [What we should use directly]
- 🔧 **Adapt**: [What needs modification]
- ❌ **Avoid**: [What we shouldn't replicate and why]

## Code References
- `path/to/file.py:123` - Main implementation of [feature]
- `another/file.ts:45-67` - Usage example
- `test/file.test.js:89` - Test demonstrating behavior

## Architecture Insights
- **Pattern**: [Pattern name] used for [purpose]
- **Convention**: [Convention observed] across [components]
- **Design Decision**: [Why something was built this way]

## Recommendations
Based on this research:
1. [Actionable recommendation]
2. [Another suggestion]
3. [Potential improvement]

## Open Questions
- [Area needing more investigation]
- [Unresolved question]

## References
- Internal docs: [paths to relevant docs]
- External resources: [URLs to external resources]
- Related research: [paths to other research docs]
```

Save to: `research/YYYY-MM-DD_HH-MM-SS_topic.md`

### Step 6: Add GitHub Permalinks (if applicable)

- Check if on main branch: `git branch --show-current` and `git status`
- If on main/master or pushed, generate GitHub permalinks:
  - Get repo info: `gh repo view --json owner,name`
  - Create permalinks: `https://github.com/{owner}/{repo}/blob/{commit}/{file}#L{line}`
- Replace local file references with permalinks in the document

### Step 7: Present Findings

- Present a concise summary of findings to the user
- Include key file references for easy navigation
- Ask if they have follow-up questions or need clarification

### Step 8: Handle Follow-up Questions

- If the user has follow-up questions, append to the same research document
- Add a new section: `## Follow-up Research [timestamp]`
- Spawn new sub-agents as needed for additional investigation
- Continue updating the document

## Important Notes

- Always use parallel Task agents to maximize efficiency
- Always run fresh codebase research - never rely solely on existing documents
- Focus on finding concrete file paths and line numbers
- Research documents should be self-contained with all necessary context
- Each sub-agent prompt should be specific and focused on read-only operations
- Consider cross-component connections and architectural patterns
- Include temporal context (when the research was conducted)
- Link to GitHub when possible for permanent references
- Keep the main agent focused on synthesis, not deep file reading

## Critical Ordering

1. ALWAYS read mentioned files first before spawning sub-tasks
2. ALWAYS wait for all sub-agents to complete before synthesizing
3. ALWAYS gather metadata before writing the document
4. NEVER write the research document with placeholder values

## Sub-task Best Practices

When spawning research sub-tasks:

1. **Spawn multiple tasks in parallel** for efficiency
2. **Each task should be focused** on a specific area
3. **Provide clear instructions** including:
   - What to search for
   - Which directories to focus on
   - What information to extract
   - Expected output format
4. **Request specific file:line references** in responses
5. **Wait for all tasks to complete** before synthesizing
6. **Verify sub-task results** - spawn follow-up tasks if needed