---
name: gr-05-celery-prefetch-starvation
title: "Celery prefetch multiplier starves long-task queues"
category: queues
tags:
  - celery
  - prefetch
  - queues
confidence: high
created: "2026-04-05"
key_insight: "prefetch_multiplier>1 lets one worker hoard tasks it can't run; set to 1 for long-running task queues."
---
## Learning

Queue shows pending tasks while workers idle: hoarded prefetched messages on a busy worker.

**How to apply:** prefetch_multiplier>1 lets one worker hoard tasks it can't run; set to 1 for long-running task queues.
