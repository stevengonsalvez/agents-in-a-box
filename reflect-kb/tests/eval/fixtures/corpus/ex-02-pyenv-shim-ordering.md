---
name: ex-02-pyenv-shim-ordering
title: "pyenv shims must precede system python in PATH"
category: environment
tags:
  - pyenv
  - python
  - path
confidence: high
created: "2026-02-15"
key_insight: "If system python wins PATH resolution, pyenv version pinning silently no-ops; put ~/.pyenv/shims first."
---
## Learning

Symptom: `python --version` ignores `.python-version`. Root cause: PATH ordering. Fix: prepend pyenv shims in the shell rc.

**How to apply:** If system python wins PATH resolution, pyenv version pinning silently no-ops; put ~/.pyenv/shims first.
