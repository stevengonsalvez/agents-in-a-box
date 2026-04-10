# ClawdHub Publishing Guide for Reflect Skill

**Research Date**: 2026-01-26
**Skill**: reflect
**Target Platform**: ClawdHub (clawdhub.com)

## Executive Summary

The reflect skill is **90% compatible** with ClawdHub format. The main changes needed:
1. Restructure frontmatter to match ClawdHub conventions
2. Add `triggers` for natural language activation
3. Create `skill.json` for rich metadata
4. Simplify for broader compatibility (remove Python dependency optionally)

## Current State vs ClawdHub Requirements

### Frontmatter Comparison

| Field | Current | ClawdHub Required | Status |
|-------|---------|-------------------|--------|
| `name` | `reflect` | `reflect` | ✅ Match |
| `description` | Multi-line | Single-line preferred | ⚠️ Adjust |
| `version` | `2.0.0` | `"2.0.0"` | ✅ Match |
| `author` | Present | Not standard (use skill.json) | ⚠️ Move |
| `allowed-tools` | Present | Same format | ✅ Match |
| `triggers` | Missing | Required for activation | ❌ Add |
| `user-invocable` | Missing | Recommended | ❌ Add |
| `hooks` | Missing | Optional but valuable | ⚠️ Consider |
| `metadata.clawdbot` | Missing | Optional | ⚠️ Consider |

### Directory Structure Comparison

**Current Structure**:
```
reflect/
├── SKILL.md                      # Main skill
├── scripts/                      # Python scripts
│   ├── state_manager.py
│   ├── signal_detector.py
│   ├── metrics_updater.py
│   └── output_generator.py
├── hooks/                        # Claude Code hooks
│   ├── precompact_reflect.py
│   ├── settings-snippet.json
│   └── README.md
├── references/                   # Documentation
│   ├── signal_patterns.md
│   ├── agent_mappings.md
│   └── skill_template.md
└── assets/                       # Templates
    ├── reflection_template.md
    └── learnings_schema.yaml
```

**ClawdHub Standard Structure**:
```
reflect/
├── SKILL.md                      # Required: Main definition
├── README.md                     # Recommended: User docs
├── skill.json                    # Optional: Rich metadata
├── _meta.json                    # Auto-generated (don't create)
└── data/                         # Optional: Sub-components
    ├── signal_patterns.md
    └── agent_mappings.md
```

## Recommended Changes

### 1. Updated SKILL.md Frontmatter

```yaml
---
name: reflect
description: Self-improvement through conversation analysis. Extracts learnings from corrections and success patterns. Philosophy - Correct once, never again.
version: "2.0.0"
user-invocable: true
triggers:
  - reflect
  - self-reflect
  - review session
  - what did I learn
  - extract learnings
allowed-tools:
  - Read
  - Write
  - Edit
  - Grep
  - Glob
  - Bash
metadata:
  clawdbot:
    emoji: "🪞"
    requires:
      bins: ["python3"]
    config:
      requiredEnv: []
      stateDirs: ["~/.reflect", "{{HOME_TOOL_DIR}}/session"]
hooks:
  Stop:
    - hooks:
        - type: command
          command: "echo \"$(date -Iseconds) session_end\" >> ~/.reflect/session-log.txt"
---
```

### 2. New skill.json for Rich Metadata

```json
{
  "name": "Reflect - Agent Self-Improvement",
  "emoji": "🪞",
  "description": "Self-improvement through conversation analysis. Extracts learnings from corrections, success patterns, and session outcomes. Encodes learnings permanently into agent definitions for continuous improvement across all future sessions.",
  "category": "meta",
  "author": "Claude Code Toolkit",
  "version": "2.0.0",
  "readme": "README.md",
  "tags": [
    "reflection",
    "self-improvement",
    "learning",
    "meta",
    "agent-development",
    "continuous-improvement"
  ],
  "features": [
    "Signal detection with confidence levels (HIGH/MEDIUM/LOW)",
    "Automatic category classification (Code Style, Architecture, Process, Domain, Tools)",
    "Agent file updates with git versioning",
    "New skill generation from debugging discoveries",
    "Metrics tracking and improvement statistics",
    "Human-in-the-loop approval workflow",
    "PreCompact hook integration for Claude Code"
  ],
  "repository": "https://github.com/stevengonsalvez/ai-coder-rules",
  "license": "MIT",
  "keywords": [
    "reflect",
    "self-improvement",
    "learning",
    "agent",
    "corrections",
    "patterns"
  ],
  "defaults": {
    "state_dir": "~/.reflect",
    "auto_reflect": false,
    "confidence_threshold": "medium"
  },
  "clawdbot": {
    "requires": {
      "python": true
    }
  }
}
```

### 3. Create README.md for User Documentation

Create a user-friendly README.md that explains:
- What the skill does
- How to use it (commands/triggers)
- Configuration options
- Examples

### 4. Reorganize for ClawdHub Compatibility

**Option A: Keep Python Scripts (Full Feature Set)**
```
reflect/
├── SKILL.md
├── README.md
├── skill.json
├── scripts/                      # Keep for advanced features
│   ├── signal_detector.py
│   ├── state_manager.py
│   ├── metrics_updater.py
│   └── output_generator.py
└── data/                         # Move references here
    ├── signal_patterns.md
    ├── agent_mappings.md
    └── skill_template.md
```

**Option B: Pure Markdown (Maximum Compatibility)**
```
reflect/
├── SKILL.md                      # Self-contained with all logic
├── README.md
├── skill.json
└── data/
    ├── signal_patterns.md
    ├── agent_mappings.md
    └── templates/
        ├── reflection_output.md
        └── new_skill.md
```

### 5. Publishing Checklist

- [ ] Update SKILL.md frontmatter with `triggers` and `user-invocable`
- [ ] Create skill.json with rich metadata
- [ ] Create README.md for user documentation
- [ ] Move reference files to `data/` directory
- [ ] Test locally with `clawdhub` CLI
- [ ] Submit PR to `clawdbot/skills` repository

## Publishing Process

### 1. Register with ClawdHub

```bash
# Install ClawdHub CLI
npm install -g clawdhub

# Authenticate
clawdhub login
```

### 2. Validate Skill

```bash
# From skill directory
clawdhub validate .
```

### 3. Publish

```bash
# Single skill publish
clawdhub publish . --slug reflect --name "Agent Reflection" --version 2.0.0 --tags latest
```

### 4. Verify

```bash
# Check publication
clawdhub info reflect

# Test installation
clawdhub install reflect
```

## Unique Value Proposition

The reflect skill offers features not found in other ClawdHub skills:

1. **Learning Persistence**: Unlike one-off reflection, learnings are encoded into agent definitions
2. **Signal Detection**: Automated pattern matching for corrections with confidence levels
3. **Category Classification**: Intelligent routing to appropriate agent files
4. **Skill Generation**: Can create new skills from debugging discoveries
5. **Metrics Tracking**: Quantified improvement with acceptance rates
6. **Human-in-the-Loop**: All changes require explicit approval
7. **Git Integration**: Full version control with rollback capability

## Competitive Analysis

| Feature | reflect | planning-with-files | personas |
|---------|---------|---------------------|----------|
| Self-improvement | ✅ Core | ❌ | ❌ |
| Learning persistence | ✅ | ✅ (plans) | ❌ |
| Agent updates | ✅ | ❌ | ❌ |
| Skill generation | ✅ | ❌ | ✅ (personas) |
| Metrics tracking | ✅ | ❌ | ❌ |
| Hook integration | ✅ | ✅ | ❌ |

## Marketing Copy

**Title**: Reflect - Agent Self-Improvement Skill

**Tagline**: "Correct once, never again"

**Description**:
> Transform your AI assistant into a continuously improving partner. The reflect skill
> analyzes conversations for corrections and successful patterns, permanently encoding
> learnings into agent definitions. Every mistake becomes an improvement that persists
> across all future sessions.

**Key Benefits**:
- Learn from corrections automatically
- Track improvement with metrics
- Create new skills from discoveries
- Version control all changes
- Human-approved updates only

## Sources

- [ClawdHub Documentation](https://docs.clawd.bot/tools/clawdhub)
- [ClawdHub Skills Repository](https://github.com/clawdbot/skills)
- [ClawdHub Marketplace](https://clawdhub.com/skills)
- [VoltAgent Awesome Clawdbot Skills](https://github.com/VoltAgent/awesome-clawdbot-skills)
