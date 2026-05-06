# Security Policy

## Reporting a vulnerability

If you find a security issue in **agents-in-a-box**, please **do not open a public GitHub issue**.

Instead, email **steven.gonsalvez@gmail.com** with:

- A description of the issue
- Steps to reproduce (PoC if available)
- The affected component (bootstrap, a specific skill, a plugin, etc.)
- Your assessment of severity

We aim to respond within 72 hours and to publish a fix + advisory within 14 days for critical issues.

## Scope

This repo distributes **agent skills, plugins, and rules** that get installed into a user's local agent config (`~/.claude/`, `~/.codex/`, etc.). Security issues we care about:

- **Skills or hooks that exfiltrate credentials** — env vars, `.env` files, SSH keys, API tokens leaving the user's machine
- **Skills that execute arbitrary code from untrusted sources** — fetching + running scripts without integrity checks
- **Bootstrap install paths that allow path traversal** — a malicious plugin manifest writing outside the target dir
- **Marketplace plugin manifest issues** — a plugin that claims one component set but ships another
- **Unsafe template substitution** — placeholder injection that escapes the intended dir

## Out of scope

- Issues in third-party plugins, skills, or CLIs we reference but don't author (`reflect-kb`, `beads`, `mcporter`, etc.) — report those upstream.
- The user's own `~/.claude/settings.json` or local hook scripts they wrote.
- Bugs in the agent runtimes themselves (Claude Code, Codex CLI, etc.).

## Supported versions

The `main` branch is the only supported version. There are no separate release lines yet. If/when this changes, this section will be updated.
