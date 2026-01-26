# Reflect Learning Plugin

> "Correct once, never again."

Transform your AI assistant into a continuously improving partner through self-reflection and learning from corrections.

## Features

- **Signal Detection**: Automatically identifies corrections with confidence levels (HIGH/MEDIUM/LOW)
- **Category Classification**: Routes learnings to appropriate agent files (Code Style, Architecture, Process, Domain, Tools)
- **Skill Generation**: Creates new skills from non-trivial debugging discoveries
- **Metrics Tracking**: Quantifies improvement with acceptance rates and statistics
- **Human-in-the-Loop**: All changes require explicit approval
- **Git Integration**: Full version control with easy rollback

## Installation

```bash
claude plugin install reflect-learning@agents-in-a-box
```

## Usage

### Quick Commands

| Command | Action |
|---------|--------|
| `reflect` | Analyze conversation for learnings |
| `reflect on` | Enable auto-reflection |
| `reflect off` | Disable auto-reflection |
| `reflect status` | Show state and metrics |
| `reflect review` | Review pending learnings |

### When to Use

- After completing complex tasks
- When user explicitly corrects behavior ("never do X", "always Y")
- At session boundaries or before context compaction
- When successful patterns are worth preserving

## Philosophy

Every correction becomes a permanent improvement that persists across all future sessions. The reflect skill analyzes conversations for correction signals and successful patterns, permanently encoding learnings into agent definitions.

## License

Apache-2.0
