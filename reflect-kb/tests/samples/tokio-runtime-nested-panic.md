---
title: "Tokio runtime panic on nested block_on"
category: build-errors
tags:
  - rust
  - tokio
  - async
  - runtime
symptoms:
  - "Cannot start a runtime from within a runtime"
  - "thread 'main' panicked at 'Cannot block the current thread'"
root_cause: "Calling block_on() or Runtime::new() inside an already running async context"
key_insight: "Use tokio::task::spawn_blocking for sync code, or restructure to avoid nesting runtimes"
created: "2026-02-11"
confidence: high
language: rust
framework: tokio
---

## Problem

When running async Rust code with Tokio, you may encounter this panic:

```
thread 'main' panicked at 'Cannot start a runtime from within a runtime.
This happens because a function (like `block_on`) attempted to block the
current thread while the thread is being used to drive asynchronous tasks.'
```

## Solution

Use `tokio::task::spawn_blocking` for sync code, or restructure to avoid
nesting runtimes.
