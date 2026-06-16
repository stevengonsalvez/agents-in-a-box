---
name: ex-05-gh-graphql-pagination
title: "gh api graphql pagination needs explicit cursor loops"
category: github
tags:
  - gh-cli
  - graphql
  - pagination
confidence: high
created: "2026-03-18"
key_insight: "gh api graphql has no --paginate for nested connections; loop on pageInfo.endCursor manually."
---
## Learning

--paginate only works for REST. GraphQL connections need a while loop reading hasNextPage/endCursor.

**How to apply:** gh api graphql has no --paginate for nested connections; loop on pageInfo.endCursor manually.
