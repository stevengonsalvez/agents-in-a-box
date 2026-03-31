---
name: session-summary
description: Generate a comprehensive session summary
user-invocable: true
argument-hint: [slug]
---

Generate a comprehensive session summary: $ARGUMENTS

Follow these steps:

1. **Create session summary file**:
   - Generate filename as `session_{slug}_{timestamp}.md` where:
     - `{slug}` is a short description of the session topic
     - `{timestamp}` is current date/time in YYYY-MM-DD-HHMMSS format
   - If $ARGUMENTS provides a specific filename or slug, use that instead

2. **Gather session information**:
   - Review the conversation history and actions taken
   - Identify key accomplishments and deliverables
   - Note any challenges encountered and how they were resolved
   - Document tools and commands used during the session

3. **Analyze session metrics**:
   - Count the total number of conversation turns
   - Estimate the total cost of the session (if available)
   - Assess the efficiency of the workflow
   - Identify any performance bottlenecks or delays

4. **Document the session summary with**:
   - **Session Overview**: Brief description of the session purpose and goals
   - **Key Actions**: Bullet-point list of major actions taken
   - **Deliverables**: Files created, modified, or analyzed
   - **Tools Used**: List of Claude Code tools and external commands used
   - **Metrics**:
     - Total conversation turns
     - Session duration (if available)
     - Estimated cost
   - **Efficiency Insights**: What went well and what could be improved
   - **Process Improvements**: Suggestions for future sessions
   - **Interesting Observations**: Notable patterns, discoveries, or highlights

5. **Save and organize**:
   - Save the summary in the current working directory
   - Include links to any files created or modified during the session
   - Add timestamps for when major milestones were achieved

Remember: Focus on actionable insights and concrete outcomes rather than just listing activities.
