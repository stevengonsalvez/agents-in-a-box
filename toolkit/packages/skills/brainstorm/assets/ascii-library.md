# ASCII Preview Library

Reference templates for `/brainstorm`. Pick the slice that matches your subject type and embed it into the topic-stub's "ASCII preview library" section. Keep each ≤ 80 chars wide. Caveman inside boxes (short technical terms, never sentences).

## UI (web/mobile)

```
┌──────────────────────────────────────┐
│ ☰  Logo            Sign in  [Get →]  │  ← header
├──────────────────────────────────────┤
│  Headline goes here                  │
│  Subtext under headline              │
│                                      │
│  ┌────────────┐  ┌────────────┐      │
│  │ Feature 1  │  │ Feature 2  │      │
│  └────────────┘  └────────────┘      │
│                                      │
│  [ Primary CTA ]   [ Secondary ]     │
└──────────────────────────────────────┘
```

Split-pane variant:
```
┌─────────────────────────────────────┐
│ ┌────────┐ │  Detail pane           │
│ │ List 1 │ │  ─────────             │
│ │ List 2 │ │  Body content...       │
│ │ List 3 │ │                        │
│ └────────┘ │                        │
└─────────────────────────────────────┘
```

## TUI (terminal UI)

```
┌─ Sessions ─────────────────────────────┐
│ ▶ session-1     running   ainb-tui     │
│   session-2     idle      website      │
│   session-3     exited    research     │
│                                        │
│ [Space] select  [Enter] open  [q] quit │
└────────────────────────────────────────┘
```

Two-pane TUI:
```
┌─ List ────────┬─ Detail ──────────────┐
│ ▶ item-1      │ id:     abc123        │
│   item-2      │ status: running       │
│   item-3      │ since:  10m ago       │
│               │ logs:   ...           │
└───────────────┴───────────────────────┘
status: 3 items │ filter: all │ q quit
```

Defer to `tui-style-guide` skill for chars and conventions.

## API (REST resource)

```
GET    /v1/sessions          → list, paginated
POST   /v1/sessions          → create
GET    /v1/sessions/:id      → fetch one
PATCH  /v1/sessions/:id      → update status
DELETE /v1/sessions/:id      → terminate

POST /v1/sessions
{
  "agent":  "claude",
  "cwd":    "/path/to/project",
  "title":  "string"
}
→ 201 Created
{ "id": "uuid", "status": "running", "created_at": "..." }
```

GraphQL variant:
```
type Session {
  id:        ID!
  agent:     AgentKind!
  status:    SessionStatus!
  createdAt: DateTime!
}
type Mutation {
  createSession(input: CreateSessionInput!): Session!
}
```

## Data model

```
┌─ User ────────┐         ┌─ Session ─────────┐
│ id      uuid  │──1:N──▶ │ id        uuid    │
│ email   str   │         │ user_id   fk      │
│ created ts    │         │ agent     enum    │
└───────────────┘         │ status    enum    │
                          │ started   ts      │
                          │ ended     ts?     │
                          └─────────┬─────────┘
                                    │ 1:N
                                    ▼
                          ┌─ Message ─────────┐
                          │ id        uuid    │
                          │ session_id fk     │
                          │ role      enum    │
                          │ body      text    │
                          │ ts        ts      │
                          └───────────────────┘
```

## Architecture (component + dataflow)

```
┌─────────┐    HTTPS    ┌──────────┐    pgwire   ┌──────────┐
│ Browser │────────────▶│ Edge Fn  │────────────▶│ Postgres │
└─────────┘             └────┬─────┘             └──────────┘
                             │
                             │  webhook
                             ▼
                       ┌──────────┐
                       │  Queue   │
                       └────┬─────┘
                            │ poll
                            ▼
                       ┌──────────┐
                       │ Worker   │
                       └──────────┘
```

## CLI (commands + sample output)

```
$ tool create --name foo --type bar
✓ created  id=abc123  status=pending

$ tool list
NAME  TYPE  STATUS    AGE
foo   bar   running   2m
baz   bar   exited    5h

$ tool logs foo --since 1m
12:01:02  starting up
12:01:03  bound :8080
12:01:04  ready
```

## Config (schema variants)

```yaml
# Option A — flat
service:
  name:    foo
  retries: 3
  timeout: 30s
```
```yaml
# Option B — nested with policy block
service:
  name: foo
  retry_policy:
    max:     3
    backoff: exponential
    jitter:  true
  timeout: 30s
```

## State machine

```
[pending] ──start──▶ [running] ──ok──▶ [done]
                         │
                         ├──error──▶ [failed] ──retry──▶ [pending]
                         │
                         └──cancel─▶ [cancelled]
```
