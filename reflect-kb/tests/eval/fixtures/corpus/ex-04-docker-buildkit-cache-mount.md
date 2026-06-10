---
name: ex-04-docker-buildkit-cache-mount
title: "BuildKit cache mounts need explicit mode for non-root users"
category: docker
tags:
  - docker
  - buildkit
  - caching
confidence: medium
created: "2026-04-02"
key_insight: "RUN --mount=type=cache defaults to root-owned; pass uid/gid/mode or chown in the same layer for non-root builds."
---
## Learning

Non-root images fail with EACCES on the cache dir. The mount is root-owned by default.

**How to apply:** RUN --mount=type=cache defaults to root-owned; pass uid/gid/mode or chown in the same layer for non-root builds.
