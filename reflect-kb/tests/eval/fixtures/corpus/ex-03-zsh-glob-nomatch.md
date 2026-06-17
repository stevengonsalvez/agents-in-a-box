---
name: ex-03-zsh-glob-nomatch
title: "zsh aborts scripts on unmatched globs — use null_glob or quote"
category: shell
tags:
  - zsh
  - globbing
  - scripting
confidence: high
created: "2026-01-20"
key_insight: "zsh raises 'no matches found' and aborts where bash passes the literal pattern; set null_glob or quote the pattern."
---
## Learning

`rm -f *.tmp` fails in zsh when no .tmp files exist. bash passes the literal; zsh errors. Fix: `setopt null_glob` or quote and test.

**How to apply:** zsh raises 'no matches found' and aborts where bash passes the literal pattern; set null_glob or quote the pattern.
