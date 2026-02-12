---
title: "Tokio runtime panic on nested block_on"
category: build-errors
tags: [rust, tokio, async, runtime]
symptoms:
  - "Cannot start a runtime from within a runtime"
  - "thread 'main' panicked at 'Cannot block the current thread'"
root_cause: "Calling block_on() or Runtime::new() inside an already running async context"
key_insight: "Use tokio::task::spawn_blocking for sync code, or restructure to avoid nesting runtimes"
related: []
created: 2026-02-11
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

This typically happens when:
1. You call `runtime.block_on()` inside an async function
2. A sync library internally creates a new runtime
3. Nested `#[tokio::main]` or `#[tokio::test]` attributes

## Solution

### Option 1: Use spawn_blocking for sync code

```rust
// Instead of this (WRONG):
async fn process() {
    let result = some_sync_blocking_call();  // Might create runtime internally
}

// Do this (CORRECT):
async fn process() {
    let result = tokio::task::spawn_blocking(|| {
        some_sync_blocking_call()
    }).await.unwrap();
}
```

### Option 2: Pass the runtime handle

```rust
// Instead of creating a new runtime:
fn sync_function() {
    let rt = tokio::runtime::Runtime::new().unwrap();  // WRONG if already in runtime
    rt.block_on(async_work());
}

// Accept a handle from the caller:
fn sync_function(handle: &tokio::runtime::Handle) {
    handle.block_on(async_work());  // Uses existing runtime
}
```

### Option 3: Restructure to stay async

```rust
// If you're already async, stay async:
async fn caller() {
    // Don't block, just await
    let result = async_work().await;
}
```

## Context

This is a common issue when:
- Using libraries that weren't designed for async (they may create their own runtime)
- Mixing sync and async code without proper boundaries
- Testing async code with `#[tokio::test]` and calling blocking test utilities

## See Also

- [Tokio docs on blocking](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html)
- [async-std vs tokio runtime conflicts](https://rust-lang.github.io/async-book/)
