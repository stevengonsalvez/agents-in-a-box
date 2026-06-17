---
name: gr-04-nginx-keepalive-upstream
title: "nginx upstream keepalive needs explicit Connection header"
category: infrastructure
tags:
  - nginx
  - keepalive
  - http
confidence: medium
created: "2026-03-25"
key_insight: "Set proxy_http_version 1.1 and clear the Connection header or upstream keepalive silently never engages."
---
## Learning

Without it every request opens a fresh upstream TCP connection, magnifying timeout pressure.

**How to apply:** Set proxy_http_version 1.1 and clear the Connection header or upstream keepalive silently never engages.
