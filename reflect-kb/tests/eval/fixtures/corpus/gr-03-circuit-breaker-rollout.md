---
name: gr-03-circuit-breaker-rollout
title: "Circuit breaker stops gateway cascade amplification"
category: architecture
tags:
  - circuit-breaker
  - resilience
confidence: medium
created: "2026-03-20"
key_insight: "A breaker in front of the flaky dependency converts cascade failures into fast, bounded degradation."
---
## Learning

Half-open probes restore service automatically once the dependency recovers.

**How to apply:** A breaker in front of the flaky dependency converts cascade failures into fast, bounded degradation.
