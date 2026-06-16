---
name: ex-07-sqlite-wal-checkpoint
title: "SQLite WAL grows unbounded without checkpointing"
category: database
tags:
  - sqlite
  - wal
  - operations
confidence: medium
created: "2026-05-01"
key_insight: "Long-lived readers block WAL checkpoints; run PRAGMA wal_checkpoint(TRUNCATE) on a writer connection periodically."
---
## Learning

A -wal file growing to GBs means checkpoint starvation. Close long readers or checkpoint explicitly.

**How to apply:** Long-lived readers block WAL checkpoints; run PRAGMA wal_checkpoint(TRUNCATE) on a writer connection periodically.
