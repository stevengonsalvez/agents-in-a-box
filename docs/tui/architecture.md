---
title: "TUI architecture"
---

# TUI architecture

> **Status:** stub. Authoritative content currently lives at `ainb-tui/CLAUDE.md + ainb-tui/src/ source tree`.
> Migration of that file into this path is gated on Stevie's approval.

How the 115-module Rust codebase is organised, including the plugin runtime.

## What this page will contain

- Crate layout (`ainb-core`, `ainb-plugin-runtime`, `ainb-plugin-sdk-rust`, …)
- Module tree (`app/`, `components/`, `widgets/`, `docker/`, `tmux/`, `git/`, `claude/`)
- Event flow + state machine
- Plugin runtime hookup (where the v2 ABI plugs in)
- Style guide reference (cornflower/gold palette, BorderType::Rounded)
- Testing surface (unit · VT100 · E2E PTY · tripwire)

## See also

- [Docs hub](../README.md)
