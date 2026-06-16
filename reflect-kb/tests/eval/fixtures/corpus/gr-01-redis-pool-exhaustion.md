---
name: gr-01-redis-pool-exhaustion
title: "Redis connection pool exhaustion under burst load"
category: infrastructure
tags:
  - redis
  - connection-pool
  - scaling
confidence: high
created: "2026-03-10"
key_insight: "Default pool of 10 connections exhausts under burst; size the pool to peak concurrency and add a wait timeout."
---
## Learning

Symptom: intermittent ConnectionError under load while Redis itself is healthy. The client pool is the bottleneck.

**How to apply:** Default pool of 10 connections exhausts under burst; size the pool to peak concurrency and add a wait timeout.
