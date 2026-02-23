---
name: session-metrics
description: View detailed metrics about the current session
user-invocable: true
---

# Session Metrics Command

Use this command to view detailed metrics about the current session.

## Usage

```
/session-metrics
```

## Description

This command provides detailed analytics about:

- Message count and conversation flow
- Task completion rates
- File modification statistics
- Code quality metrics
- Session duration and productivity

## Metrics Displayed

- **Messages**: Current count and percentage of limit
- **Tasks**: Completed vs pending todos
- **Files**: Files modified, lines changed
- **Commands**: Most used commands this session
- **Productivity**: Tasks completed per hour

## Example Output

```
Session Metrics
Messages: 38/50 (76%)
Tasks: 12 completed, 3 pending
Files: 8 modified, 247 lines changed
Top Commands: /edit (15), /create (8), /run (5)
Duration: 2.3 hours
```
