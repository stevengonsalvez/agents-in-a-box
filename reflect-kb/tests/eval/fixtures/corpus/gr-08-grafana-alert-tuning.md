---
name: gr-08-grafana-alert-tuning
title: "Alert on memory slope, not absolute threshold"
category: observability
tags:
  - grafana
  - alerting
  - memory
confidence: medium
created: "2026-04-15"
key_insight: "Slope-based alerts (deriv over 30m) catch leaks days before any absolute threshold fires."
---
## Learning

Absolute thresholds fire at 2am when it's too late; the slope was visible for days.

**How to apply:** Slope-based alerts (deriv over 30m) catch leaks days before any absolute threshold fires.
