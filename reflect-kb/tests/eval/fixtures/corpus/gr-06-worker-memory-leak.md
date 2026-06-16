---
name: gr-06-worker-memory-leak
title: "Worker RSS creep traced to prefetched task payloads"
category: queues
tags:
  - memory
  - worker
  - celery
confidence: medium
created: "2026-04-08"
key_insight: "Prefetched payloads held in worker memory between task runs leak RSS; max-tasks-per-child recycles cleanly."
---
## Learning

RSS grows linearly with prefetched backlog. Recycling workers bounds it.

**How to apply:** Prefetched payloads held in worker memory between task runs leak RSS; max-tasks-per-child recycles cleanly.
