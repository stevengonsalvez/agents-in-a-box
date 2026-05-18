# Architecture deep-dive

> **Status:** stub. Authoritative content currently lives at `docs/product/architecture.md + ainb-tui/CLAUDE.md`.
> Migration of that file into this path is gated on Stevie's approval.

Whole-monorepo architecture: every component, every boundary, every data flow.

## What this page will contain

- Components recap
- Boundary contracts
- Data flow walkthroughs (session start, plugin spawn, snapshot fan-out, reflection capture)
- On-disk layout (~/.agents-in-a-box/, ~/.claude/, ~/.reflect/, dist/plugins/)
- Why the choices we made

## See also

- [Docs hub](../README.md)
