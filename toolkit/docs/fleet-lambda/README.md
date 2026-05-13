# Fleet Lambda Overlay Docs

This directory holds the tracked planning/docs layer for Fleet Lambda overlay cleanup.

## Files
- `overlay-ownership-matrix.md` — current artifact classification and boundary rules
- `bank-provenance-design.md` — observability design for BANK lookups, injections, and correction joins
- `migration-plan.md` — phased migration path from runtime/backup drift to clean repo ownership

## Operating rule

Runtime paths are deployment targets.
Code repos are source-of-truth.
Backup repos hold non-code/manual state only.
