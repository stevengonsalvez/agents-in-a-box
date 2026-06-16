---
name: tp-02-auth-session-convention
title: "Auth convention: server-side sessions replace JWT"
category: conventions
tags:
  - auth
  - sessions
  - convention
confidence: high
created: "2026-05-15"
key_insight: "As of 2026-05 the team standard is server-side sessions with a shared session store; JWTs are deprecated for first-party auth."
---
<!-- archived: 2026-05-15T00:00:00 -->

## Learning

Revocation pain and key-rotation incidents drove the move off JWTs. This supersedes the 2025 JWT convention.

**How to apply:** As of 2026-05 the team standard is server-side sessions with a shared session store; JWTs are deprecated for first-party auth.
