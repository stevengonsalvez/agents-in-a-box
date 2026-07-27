# Workspace / Membership / Tenancy: Multica vs Hangar

## 1. Multica Workspace

**Schema** (`server/migrations/001_init.up.sql:15-33`):

```sql
CREATE TABLE workspace (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    slug TEXT UNIQUE NOT NULL,
    description TEXT,
    settings JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE member (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('owner', 'admin', 'member')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(workspace_id, user_id)
);
```

Columns bolted on by later migrations (still same `workspace` row, all nullable/defaulted, non-breaking):
- `context` TEXT — `006_workspace_context.up.sql:1`. The **workspace-level agent system prompt**: every agent in the workspace reads it (docs/product-overview.md:120,138).
- `repos` JSONB DEFAULT '[]' — `014_workspace_repos.up.sql:1`. Git-repo allow-list an agent task may check out (docs:138, "仓库白名单").
- `avatar_url` TEXT — `111_workspace_avatar.up.sql:1`.
- `attribution_fail_closed` BOOLEAN NOT NULL DEFAULT FALSE — `188_workspace_attribution_fail_closed.up.sql`. Compliance knob: when true, an agent run that can't resolve to a precise accountable human is refused at enqueue instead of degrading to "owner_fallback".
- `issue_prefix` (referenced throughout handler code, e.g. `workspace.go:139` `IssuePrefix string`) — per-workspace issue numbering prefix (e.g. `ACME-42`), auto-derived from the name (`generateIssuePrefix`, `workspace.go:24-34`) or caller-supplied at create.

`settings` JSONB is a free-form escape hatch (no fixed shape found in this pass beyond the typed columns above absorbing the concrete knobs — `settings` itself appears unused for anything with a fixed contract in the handler; typed columns (`context`, `repos`, `issue_prefix`, `attribution_fail_closed`, `avatar_url`) are where real config actually lives).

**Membership model** (`workspace.go:83-95`, `168-256`):
- Roles: `owner` / `admin` / `member`, closed set, enforced in Postgres CHECK **and** re-validated in Go (`normalizeMemberRole`, `workspace.go:311-321`).
- **Last-owner invariant**: `UpdateMember`/`DeleteMember`/`LeaveWorkspace` all call `countOwners(members) <= 1` before demoting/removing/self-removing an owner and reject with 400 "workspace must have at least one owner" (`workspace.go:398-406, 456-464, 496-504`).
- Only an existing `owner` can promote/demote to/from `owner` (`workspace.go:397-401` for members, `385-388` for create).
- `ListMembers` / `ListMembersWithUser` (`workspace.go:262-322`) — the latter joins `user` for name/email/avatar, the display shape the Members settings pane renders.
- Membership is workspace-scoped throughout: every mutation resolves the requester via `h.workspaceMember`/`requireWorkspaceMember` first (403/404 on a foreign workspace), and a `MembershipCache` is invalidated on every role/removal change (`workspace.go:441,...`).

**Invites** — `CreateMember` in `workspace.go` (name kept for the route, but per `invitation.go:52-56` it "replaces the old instant-add CreateMember flow") is superseded by `CreateInvitation` (`invitation.go`):
- `POST /api/workspaces/{id}/members` now creates a `workspace_invitation` row, not an instant member.
- Schema (`041_workspace_invitation.up.sql`): `workspace_id`, `inviter_id`, `invitee_email`, `invitee_user_id` (nullable — filled once the invitee has an account), `role` (admin|member — **cannot invite as owner**, `invitation.go` rejects it explicitly), `status` (pending/accepted/declined/expired), `expires_at` default now()+7 days.
- One live pending invite per `(workspace_id, invitee_email)` — partial unique index `idx_invitation_unique_pending ... WHERE status='pending'`. Stale pending invites are expired first (`ExpireStalePendingInvitations`) before a re-invite, working around #2055.
- Old `CreateMember`/`CreateInvitation` still auto-create a stub `user` row by email if none exists (`workspace.go:326-333`, "Auto-create user with email so they can be invited before signing up").
- Zero-workspace invite acceptance: an invited user who has no workspace of their own skips onboarding entirely and lands directly in the invited workspace (docs:641-648).

**Agent-as-member modeling** — NOT literally a `member` row. Multica's "agents are team members" design is the **polymorphic actor pattern**: any "who did this" column is a pair `actor_type ('member'|'agent')` + `actor_id`, not a shared identity table (docs:121, "几乎所有'谁做了什么'的字段都是 actor_type + actor_id"). Concretely, from `001_init.up.sql`:
- `issue.assignee_type CHECK (assignee_type IN ('member','agent'))` (line 61) — an issue can be assigned to a human member OR an agent, same column shape.
- `issue.creator_type CHECK (creator_type IN ('member','agent'))` (line 63) — an agent can create issues.
- `comment.author_type CHECK (author_type IN ('member','agent'))` (line 100) — agent comments render the same as human comments.
- `inbox_item.recipient_type CHECK (recipient_type IN ('member','agent'))` (line 113) — **agents get an inbox too**.
- `activity_log.actor_type CHECK (actor_type IN ('member','agent','system'))` (line 160).
- Squad "lead" is likewise polymorphic member-or-agent (docs:224).
So an `agent` is its own top-level table (`agent`, lines 36-49: `workspace_id`, `name`, `runtime_mode`, `visibility`, `status`, `owner_id`, …) — agents are NOT rows in `member` — but every collaboration surface (assignment, authorship, comments, inbox, activity, @-mentions in the Tiptap editor per docs:190) is typed to accept `member` OR `agent` interchangeably. That symmetric plumbing is the actual "agents are team members, not tools" claim.

**Create-workspace inputs** (`CreateWorkspaceRequest`, `workspace.go:150-156`): `name`, `slug` (validated `^[a-z0-9]+(-[a-z0-9]+)*$`, checked against a shared reserved-slugs list embedded from `reserved_slugs.json` so it can never collide with a top-level frontend route), optional `description`, `context`, `issue_prefix` (else auto-derived from `name`). `CreateWorkspace` (`workspace.go:158-227`):
1. Gated by `DISABLE_WORKSPACE_CREATION` operator flag (self-host lockdown, 403 for everyone including existing owners).
2. One transaction: insert `workspace` row, then insert a `member` row for the creator with `role='owner'`.
3. Deliberately does NOT mark the user onboarded (`onboarded_at` is owned by a separate `CompleteOnboarding` step / by `AcceptInvitation`) — "has a workspace" and "finished setup" are decoupled.
4. Emits `WorkspaceCreated` analytics event + notifies the daemon of workspace-list change.

Frontend (`packages/core/workspace/mutations.ts`): `useCreateWorkspace` seeds the workspace-list query cache synchronously in `onSuccess` (so navigating to `/{slug}/issues` never flashes a loading state), `useDeleteWorkspace`/`useLeaveWorkspace` invalidate the list on settle. Multi-workspace switching is "pure navigation" — `/{workspaceSlug}/...` URL shape, `X-Workspace-Slug` header on every API call (docs:143).

## 2. Hangar Workspace (ainb)

**Schema** (`crates/ainb-hangar-store/migrations/0001_init_workspace_user_member.sql:10-28`):

```sql
CREATE TABLE workspace (
    id         TEXT PRIMARY KEY,
    slug       TEXT NOT NULL UNIQUE,
    name       TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE user (
    id         TEXT PRIMARY KEY,
    email      TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL
);

CREATE TABLE member (
    workspace_id TEXT NOT NULL REFERENCES workspace(id),
    user_id      TEXT NOT NULL REFERENCES user(id),
    role         TEXT NOT NULL CHECK (role IN ('owner','admin','member')),
    PRIMARY KEY (workspace_id, user_id)
);
```

Migration 0001's own header comment is explicit about intent: "multi-tenancy is a day-1 primitive… `user` + `member` model the single-user-at-v1 reality without a refactor cost when multi-user lands" (`0001_init_workspace_user_member.sql:1-8`) — i.e. the tables mirror multica's shape on purpose, but nothing downstream exercises multi-user or multi-workspace yet.

Extra columns added by migration 0020 (`0020_workspace_config.sql`), explicitly modeled as the hangar analog of multica's `context`/`repos`/`issue_prefix`:
- `context_prompt` TEXT nullable — same role as multica's `workspace.context`: injected as a `CLAUDE.md` into every task's execenv.
- `repo_whitelist` TEXT nullable (JSON array string) — same role as multica's `workspace.repos`; **persisted and validated but the checkout-flow gate that consumes it does not exist yet** (migration comment: "A repo-checkout flow does not exist yet").
- `issue_prefix` TEXT nullable — prepended to a new issue's title.
No `settings` JSONB, no `description`, no `avatar_url`, no `attribution_fail_closed` equivalent.

**Bootstrap / creation** (`crates/ainb-hangar-store/src/bootstrap.rs`): there is **no `CreateWorkspace` request path at all**. `ensure_default_workspace` (lines 148-186) lazily inserts exactly one workspace (slug `"default"`, name `"Default Workspace"`) + one owner user (email `"stevie@local"`, `bootstrap.rs:36`) + one `member` row with `role='owner'`, the first time any entry point touches the DB, and is a pure find-or-create idempotent singleton (tested for idempotency and for concurrent-writer convergence, lines 265-311 in bootstrap.rs's test module). Every other CLI verb defaults `--workspace` to this bootstrapped slug (`crates/ainb-core/src/cli/hangar/mod.rs:117-129` doc comment, and `--workspace: Option<String>` on essentially every subcommand's args struct).

**CLI surface** (`crates/ainb-core/src/cli/hangar/mod.rs`):
- `hangar workspace <verb>` (lines 117-135) has **only two verbs**: `config` (set `context_prompt`/`repo_whitelist`/`issue_prefix`, with `--clear-*` flags to unset) and `show`. **No `create`, `list`, `switch`, or `delete` verb exists.**
- `hangar member <verb>` originally had **only** `list`, `set-role`, `remove`. `add` landed with #1's human-member mint path, and `invite` / `invites` / `accept` / `decline` / `revoke` landed with #18 — accepting an invitation is what mints the `user` + `member` rows.
- `MemberRepo` (`crates/ainb-hangar-store/src/repo/member.rs`) is real and well-tested: `list` (join to `user` for email/role, workspace-scoped), `set_role`, `remove`, both guarding a **last-owner invariant** identical in spirit to multica's (`owner_count_in_tx <= 1` check inside the mutation transaction, `member.rs:126-167, 184-205`) and both workspace-scoped by the `(workspace_id, user_id)` composite PK (a foreign-tenant pair resolves to `NotFound`, never a cross-tenant edit — mirrors multica's `requireWorkspaceMember` pattern).
- (Superseded by #18.) At assessment time there was no invitation table, flow, or email-based join. `workspace_invitation` (migration 0063) + `repo/invitation.rs` now port the reference's lifecycle; the Members pane renders the live pending invites.

**Is there any multi-workspace UI/CLI, or human members at all?**
- The schema is multi-tenant-capable (workspace-scoped FKs everywhere, `MemberRepo` fully workspace-scoped) — this is a deliberate "no refactor cost later" bet per the migration 0001 comment.
- In practice: **single default workspace only**. No create-workspace surface exists (CLI, RPC, or TUI) — `ensure_default_workspace` is the only writer of the `workspace` table, and it is idempotent-singleton by construction (`ORDER BY created_at LIMIT 1` is how every "the workspace" lookup resolves — `find_default_workspace`, `bootstrap.rs:148`).
- Humans ARE modeled (`user` + `member` tables, real roles, real last-owner guard) but there is **no way to add a second human**: no invite flow, no `member add`, no signup/auth surface that mints a new `user` row. The one seeded owner (`stevie@local`) is effectively hardcoded as "the" user.
- No polymorphic actor pattern: grep across `ainb-hangar-store/src/repo` and `ainb-plugin-hangar/src` for `member`/`role` hits `squad.rs`, `card_parity.rs`, `task.rs`, etc., but issue/comment/inbox assignment types are NOT modeled as `member`-or-`agent` — agents are simply the thing that does the work; there is no human-assignee or human-commenter concept in the current TUI/CLI surface to be symmetric with.

## 3. GAPS

| # | Multica has | Hangar has | Gap | Effort |
|---|---|---|---|---|
| 1 | Multi-workspace: `CreateWorkspace` API + frontend nav (`/{slug}/...`), reserved-slug validation, per-instance `DISABLE_WORKSPACE_CREATION` gate | Create/delete/switch landed (#465); the lockdown gate landed as `daemon_config: workspace.creation_disabled` + the one-way `HANGAR_DISABLE_WORKSPACE_CREATION` env override (4-rest) | **No multi-workspace capability at all** — structurally the single biggest gap; everything else (invites, roles) is moot with one workspace | L |
| 2 | Full membership lifecycle: invite by email → pending → accept/decline/expire, auto-stub user, role assign at invite time | **LANDED (#18)** — `InvitationRepo` create/accept/decline/revoke/list + `expire_stale`, role fixed at invite time, `MemberRepo::add_in_tx` so accept flips the status and inserts the member in ONE transaction; `MemberRepo` list/set_role/remove and the last-owner guard unchanged | Delivery only: there is no email/notification channel, so the invite id is handed over out of band | done |
| 3 | `workspace_invitation` table, 7-day expiry, one-pending-per-email partial unique index, stale-expiry sweep | **LANDED (#18)** — migration 0063 ports all four: the table, `INVITE_TTL_MS` (7 days, computed in Rust because SQLite has no `INTERVAL`), `idx_invitation_unique_pending`, and the pre-create stale sweep that works around multica #2055 | None | done |
| 4 | Real multi-human orgs: `user` created via auth/signup, arbitrary email domains | Single hardcoded owner (`stevie@local`), no signup surface | **No real user/auth model** — hangar's "user" is a bootstrap fixture, not an account system | L (depends on whether ainb ever needs real multi-user auth, vs. staying single-operator by design) |
| 5 | Polymorphic actor (`actor_type` member\|agent) across issue/comment/inbox/activity — agents and humans are symmetric collaborators | Agents only; no human-assignee/commenter/inbox-recipient concept | **No "agents are team members" symmetry** — hangar's model is agent-centric, not actor-polymorphic | L (touches issue/comment/inbox schemas + every UI surface that renders them) |
| 6 | `workspace.context` (agent system prompt), `.repos` (checkout whitelist), `.issue_prefix`, `.avatar_url`, `.attribution_fail_closed`, `.description`, `.settings` JSONB | `context_prompt`, `repo_whitelist` (persisted, gate not wired), `issue_prefix` — direct analogs of the first three | **Config surface is ~50% ported**; missing `description`, `avatar_url`, compliance/attribution knob, and the whitelist gate is a stub | S–M (config columns are cheap; wiring the repo-checkout gate to `repo_whitelist` is the real remaining work, already flagged in the migration 0020 comment) |
| 7 | Reserved-slug list shared Go↔TypeScript via generated JSON, CI-enforced drift check | No slug validation surface (moot with one workspace, `slug` is just `"default"`) | N/A until gap #1 is addressed | S (once workspace-create exists) |
| 8 | Role-change/removal cache invalidation (`MembershipCache.Invalidate`) + realtime `member:added/updated/removed` websocket events | No realtime/event layer found for membership changes | **No live membership event stream** — minor, since single-workspace + no invites means membership rarely changes today | S |

**Ranked by impact**: #1 (no multi-workspace) > #2/#3 (no invite/second-human path) > #5 (no agent-as-member symmetry) > #4 (no real auth) > #6 (config parity gaps, mostly already tracked) > #7/#8 (follow naturally once #1 lands). #1 landed with #465/4-rest; #2 and #3 landed with #18.
