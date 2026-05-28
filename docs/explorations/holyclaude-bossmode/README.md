# HolyClaude → ainb bossmode · exploration bundle

Research + design exploration for re-backing ainb's bossmode onto **HolyClaude** (`coderluii/holyclaude:v1.2.2`) instead of ainb's own `docker/agents-dev/` image.

**Status**: research + design complete · 4 decisions still open · no code yet.
**Discussion**: [stevengonsalvez/agents-in-a-box#168](https://github.com/stevengonsalvez/agents-in-a-box/discussions/168)

## Start here

1. **[handover.md](handover.md)** — the full handover: decisions locked, open questions, architecture sketch, next actions. Read this first.

## Explainers (open in a browser)

| File | What it is |
|---|---|
| [explainers/holyclaude-ainb-integration.html](explainers/holyclaude-ainb-integration.html) | **Options paper** — 4 integration tracks scored (host-side / wasm plugin / own image / do nothing). Recommendation: Track A. |
| [explainers/bossmode-operating-modes.html](explainers/bossmode-operating-modes.html) | **8 operating modes** for containerised coding agents — topology diagrams, capability matrix, hard constraints, agent compat. |
| [explainers/cloudcli-streaming-architecture.html](explainers/cloudcli-streaming-architecture.html) | **CloudCLI deconstruction** — how `siteboon/claudecodeui` plumbs the chat pipe (in-process SDK) vs the shell pipe (PTY tunnel). |

```bash
open docs/explorations/holyclaude-bossmode/explainers/holyclaude-ainb-integration.html
```

## Research

| File | What it is |
|---|---|
| [research/2026-05-15_18-08_holyclaude-as-ainb-plugin.md](research/2026-05-15_18-08_holyclaude-as-ainb-plugin.md) | Full integration brief — what HolyClaude offers, ainb plugin-contract mapping, integration tracks, risks. |

## here.now mirrors (may expire)

- Options paper · https://sleek-dahlia-vxsn.here.now/
- Operating modes · https://lively-falcon-pfz4.here.now/
- CloudCLI architecture · https://noble-raven-krzq.here.now/

## Next session picks up at

1. Run `/interview` to lock the 4 open decisions (see handover.md § Open decisions).
2. `/plan` → `/implement`.
