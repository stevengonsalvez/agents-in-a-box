---
title: "Invalid: missing required confidence field"
category: test-fixtures
created: "2026-04-24"
---

This fixture exists to give the frontmatter validator something to reject.
The schema requires `confidence`; this file omits it so pre-commit /
validate-frontmatter must return non-zero.
