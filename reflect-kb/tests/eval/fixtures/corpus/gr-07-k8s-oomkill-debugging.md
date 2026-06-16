---
name: gr-07-k8s-oomkill-debugging
title: "OOMKilled pods: read the cgroup peak, not the pod limit"
category: kubernetes
tags:
  - kubernetes
  - oom
  - debugging
confidence: high
created: "2026-04-12"
key_insight: "kubectl describe shows the limit, not the spike; memory.peak in the cgroup explains which burst killed the pod."
---
## Learning

Exit code 137 with healthy averages means a short spike — find it in cgroup v2 memory.peak.

**How to apply:** kubectl describe shows the limit, not the spike; memory.peak in the cgroup explains which burst killed the pod.
