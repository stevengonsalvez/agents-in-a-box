---
name: gr-02-api-gateway-timeout-cascade
title: "Gateway 504 cascade traced to upstream pool waits"
category: infrastructure
tags:
  - gateway
  - timeout
  - cascade
confidence: high
created: "2026-03-12"
key_insight: "Gateway 504s during spikes were queued waits on the redis pool, not slow handlers — fix the pool, not the timeout."
---
## Learning

Tempting to raise gateway timeouts; the real fault was connection wait time upstream.

**How to apply:** Gateway 504s during spikes were queued waits on the redis pool, not slow handlers — fix the pool, not the timeout.
