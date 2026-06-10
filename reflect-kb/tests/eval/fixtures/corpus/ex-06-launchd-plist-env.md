---
name: ex-06-launchd-plist-env
title: "launchd jobs don't inherit shell environment variables"
category: macos
tags:
  - launchd
  - macos
  - environment
confidence: high
created: "2026-02-28"
key_insight: "launchd plists run with a minimal env; bake variables into EnvironmentVariables dict or wrap with a login shell."
---
## Learning

A job that works in the terminal but fails under launchd is almost always missing PATH or HOME context.

**How to apply:** launchd plists run with a minimal env; bake variables into EnvironmentVariables dict or wrap with a login shell.
