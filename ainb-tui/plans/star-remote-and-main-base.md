# Star = Remote Indicator + New Session Off Remote Default Branch

## Overview
Make a "star" (Favorite) **always** record a remote repository indicator (never a local
path), and make sessions launched **from a star** always create their worktree branched
off the remote's **default branch** (`origin/HEAD`), fetched fresh. Refuse to star a repo
that has no resolvable `origin` remote. Auto-migrate existing local-path stars to remote
on startup; drop the ones with no origin (with a notification).

## Current State Analysis

### Star creation — TWO entry points
1. **`StarSelectedWorkspace`** — `crates/ainb-core/src/app/events.rs:3266-3410` (PRIMARY).
   Triggered from the home/workspace list. It *already* tries `get_remote_url()` and
   prefers a remote indicator, **but falls back to `SourceType::LocalPath` in three
   branches**: (a) remote URL won't parse (`events.rs:3321-3329`), (b) repo has no
   `origin` (`events.rs:3330-3338`), (c) path isn't a git repo (`events.rs:3339-3347`).
   These three fallbacks are the bug — they persist local-path stars.
2. **pick_repo `^F`** — `crates/ainb-core/src/components/new_session/pick_repo.rs:646-673`
   `toggle_favorite()` (SECONDARY). Persists whatever the highlighted row's source is,
   including `RepoSource::LocalPath` for `📁` local-scan rows. No `AppState` access, so it
   can only `tracing::warn!` — it cannot surface a user notification today.

### New-session base branch
- `create_session_from_configure()` — `state.rs:5708-5889`. Resolves `repo_path`:
  `LocalPath` → path as-is; remote (`HttpsUrl`/`SshUrl`/`GithubShorthand`) →
  `clone_remote_for_configure()` into the `~/.agents-in-a-box/repos/...` cache. **Passes
  `existing_worktree = None` (`state.rs:5867`)** so *everything* funnels through the local
  worktree flow.
- `create_interactive_session()` — `state.rs:6297-6384`. With `existing_worktree = None`
  it takes the "local repo flow" and calls
  `InteractiveSessionManager::create_session(..., None /*base_branch*/, ...)`
  (`state.rs:6371-6383`).
- `session_manager.rs:217-251` `create_session(base_branch: Option<String>)` →
  `worktree_manager.create_worktree(..., base_branch.as_deref())`.
- `worktree_manager.rs:88-150` `create_worktree(base_branch: Option<&str>)`; `None` →
  `get_default_branch(repo)` (`worktree_manager.rs:407-422`) = **local** `main` → **local**
  `master` → current `HEAD` shorthand → `"main"`. `ensure_branch_exists()`
  (`worktree_manager.rs:424-445`) cuts the new branch from a **local** ref. This is the
  wrong base for star (remote) launches: a cached clone's local `main` is never
  fast-forwarded by `git fetch` (`remote_repo_manager.rs:226-242`), and `master`/`develop`
  defaults break the assumption.

### Reusable primitives (already exist)
- `RepositoryManager::open(path).get_remote_url() -> Result<Option<String>>` —
  `crates/ainb-core/src/git/repository.rs:83-91` (origin URL or `None`).
- `RemoteRepoManager::get_default_branch_name(source) -> Option<String>` —
  `remote_repo_manager.rs:152-178` (resolves `origin/HEAD` via `git ls-remote --symref`).
- `RemoteRepoManager::checkout_existing_branch_worktree(cache, wt, remote_branch)` —
  `remote_repo_manager.rs:426+` (`git worktree add -B <b> <path> origin/<b>`; collision
  suffixing + transcrypt `--no-checkout` retry). Good template for the new method.
- `RemoteRepoManager::clone_repo()` (`:184`) clones standard, reuses cache, and
  `git fetch --all --prune` on reuse (`:226`) — so `origin/*` is fresh after this call.
- `create_session_with_worktree()` — `session_manager.rs:353` consumes a pre-built
  worktree (used by the `existing_worktree = Some(...)` branch at `state.rs:6352-6367`).
- `create_worktree_from_cache()` (`remote_repo_manager.rs:248`) — **dead code**, never
  called; will be removed (it creates a *local*-ref-based branch, the wrong semantics).

## Desired End State
- Starring a repo with an `origin` → stores `HttpsUrl`/`SshUrl`/`GithubShorthand`.
- Starring a repo with no `origin` (or not a git repo) → **refused** with a notification;
  nothing written to `favorites.yaml`.
- A session launched from a star (any remote source) → its agent branch
  (`agents/...`) is cut from `origin/<default>` of a freshly-fetched cache, regardless of
  what the cache's local branches point at, and regardless of `main` vs `master` vs
  `develop`.
- Local-path picks / recents / typed paths → **unchanged** (`get_default_branch`).
- Existing `favorites.yaml`: on startup, every `LocalPath` star with an `origin` is
  rewritten to a remote indicator (alias/stats/tags preserved); originless ones are
  dropped; a single aggregated notification reports what changed.

### Discriminator
"Star-launched only" is implemented as **`RepoSource::is_remote()`** (`repo_source.rs:141`).
Because stars become always-remote, every star launch is `is_remote() == true`. A typed
`owner/repo` / URL is also remote and gets the same correct behavior — a bonus, not a
regression. Local-path rows are `is_remote() == false` and keep the old path.

## What We're NOT Doing
- Not adding a branch/ref field to `Favorite` (star stays a pure source pointer).
- Not changing base-branch behavior for local-path / recents / typed-path launches.
- Not adding a base-branch picker to the Configure screen.
- Not touching Boss/Docker session base-branch logic beyond what flows through the shared
  path.
- Not migrating on every `FavoritesStore::load()` (too many call sites + git I/O on
  render) — one startup pass only.

## Implementation Approach
Five phases across three waves. Wave 1 builds the two independent primitives (favorites
remote-derivation/migration; remote-default worktree creation). Wave 2 wires them into the
two surfaces (session creation; star toggles). Wave 3 hooks startup migration and adds
integration tests.

---

## Phase 1: Favorites — remote derivation + migration
<!-- wave: 1 | depends_on: [] | files: [crates/ainb-core/src/config/favorites_store.rs] -->

### Overview
Add a shared helper that derives a remote `Favorite` from a local repo path, and a
startup migration that rewrites/drops existing local-path stars.

### Changes Required

#### `crates/ainb-core/src/config/favorites_store.rs`
1. `pub enum DeriveFavoriteError { NotAGitRepo, NoRemote, Unparseable(String) }`.
2. `pub fn favorite_from_local_repo(alias: String, local_path: &Path) -> Result<Favorite, DeriveFavoriteError>`:
   - `RepositoryManager::open(local_path)` → `NotAGitRepo` on err.
   - `.get_remote_url()` → `Ok(Some(url))` continue; `Ok(None)`/`Err` → `NoRemote`.
   - Parse `url` into `(source_string, SourceType)`: GitHub host → `GithubShorthand`
     (`owner/repo`); else `git@`-prefixed → `SshUrl`; else `HttpsUrl`. (Mirror the
     classification already in `events.rs:3291-3320`, factored here so both star paths and
     migration share one implementation.)
   - Return `Favorite::new(alias, source_string, source_type)`.
3. `pub struct MigrationReport { pub migrated: Vec<(String,String)>, pub dropped: Vec<String> }`
   (`(alias, new_source)` and `alias`).
4. `pub fn migrate_local_to_remote(&mut self) -> MigrationReport`:
   - Iterate favorites; for each `SourceType::LocalPath`: call
     `favorite_from_local_repo(alias, Path::new(&source))`.
     - `Ok(remote_fav)` → replace `source`/`source_type` in place (preserve
       `display_name`/`description`/`tags`/`metadata`/`stats`), record in `migrated`.
     - `Err(_)` → mark for removal, record alias in `dropped`.
   - Remove dropped entries. Idempotent: a store with no `LocalPath` entries returns an
     empty report and is not mutated.
   - Caller is responsible for `save()` when the report is non-empty.

### Success Criteria
#### Automated Verification
- [ ] `cargo build -p ainb-core` succeeds.
- [ ] `cargo test -p ainb-core favorites_store` passes.
- [ ] `cargo clippy -p ainb-core -- -D warnings` clean for the changed crate.

#### Manual Verification
- [ ] N/A (covered by tests).

### Tests (in-module, behavioral)
- `migrate_noop_when_all_remote` — store with only remote entries → empty report,
  unchanged.
- `migrate_drops_localpath_without_origin` — a `LocalPath` to a temp dir that's not a git
  repo → dropped, removed from store.
- `favorite_from_local_repo_*` — use a `git init` + `git remote add origin <url>` temp
  repo (helper) to assert GitHub-shorthand and non-GitHub HTTPS/SSH classification, and
  `NoRemote`/`NotAGitRepo` errors. (git2 against a temp dir; no network.)

---

## Phase 2: RemoteRepoManager — worktree off origin default
<!-- wave: 1 | depends_on: [] | files: [crates/ainb-core/src/git/remote_repo_manager.rs] -->

### Overview
Add a method that cuts a **new** agent branch from the remote's default ref in a cached
clone, and delete the dead `create_worktree_from_cache`.

### Changes Required

#### `crates/ainb-core/src/git/remote_repo_manager.rs`
1. `pub fn create_worktree_off_remote_default(&self, cache_path: &Path, worktree_path: &Path, new_branch: &str, source: &RepoSource) -> Result<PathBuf, RemoteRepoError>`:
   - Resolve default via `self.get_default_branch_name(source)`; fall back to probing
     `origin/main` then `origin/master` in the cache (`git rev-parse --verify
     origin/<b>`); final fallback `RemoteRepoError` if none resolves.
   - `git -C <cache> fetch origin --prune` (defensive; `clone_repo` already fetched on
     reuse but a directly-supplied cache may be stale).
   - `git -C <cache> worktree add -b <new_branch> <worktree_path> origin/<default>`.
   - Reuse the collision + transcrypt `--no-checkout` retry handling already present in
     `checkout_existing_branch_worktree` (extract a shared private helper rather than
     copy-paste).
   - Return the created `worktree_path`.
2. Delete `create_worktree_from_cache` (`:248`) and its doc references (dead code).

### Success Criteria
#### Automated Verification
- [ ] `cargo build -p ainb-core` succeeds.
- [ ] `cargo test -p ainb-core remote_repo_manager` passes.
- [ ] `cargo clippy -p ainb-core -- -D warnings` clean.

#### Manual Verification
- [ ] N/A (covered by Phase 5 integration test).

### Tests
- Construct a bare-ish fixture: `git init` source with a `master` default + a commit,
  `clone` into cache, then call `create_worktree_off_remote_default(... "agents/x" ...)`
  and assert the worktree HEAD's merge-base equals `origin/master` even though there is no
  local `main`. (Phase 5 owns the full stale-local-main scenario.)

---

## Phase 3: Session creation branches off remote default for remote sources
<!-- wave: 2 | depends_on: [2] | files: [crates/ainb-core/src/app/state.rs] -->

### Overview
For remote sources, build the worktree off `origin/<default>` in the cache and pass it as
`existing_worktree = Some(...)`; local sources keep the current `None` path.

### Changes Required

#### `crates/ainb-core/src/app/state.rs` — `create_session_from_configure` (5708-5889)
- After `repo_path` resolution, branch on `snapshot.repo_source.is_remote()`:
  - **Remote**: `repo_path` is the cache. Compute `worktree_path`
    (`WorktreeManager::generate_worktree_path(session_id, cache, branch_name)` or the
    existing helper), then
    `RemoteRepoManager::create_worktree_off_remote_default(cache, worktree_path,
    &snapshot.branch_name, &snapshot.repo_source)`. Call `create_session_with_logs(...,
    existing_worktree = Some((worktree_path, cache_path)))` so
    `create_interactive_session` takes the `existing_worktree = Some` branch
    (`state.rs:6352`) → `create_session_with_worktree` (no `get_default_branch`).
  - **Local**: unchanged — `existing_worktree = None` (current behavior).
- Keep `branch_name` = `snapshot.branch_name` (the `agents/...` agent branch); it is now
  cut from `origin/<default>` instead of a local ref.

### Success Criteria
#### Automated Verification
- [ ] `cargo build -p ainb-core` succeeds.
- [ ] `cargo clippy -p ainb-core -- -D warnings` clean.

#### Manual Verification
- [ ] Launch a session from a starred GitHub repo whose default is `master`; new worktree
  branch's base is `origin/master` (verify `git merge-base HEAD origin/master` == tip).
- [ ] Launch from a star where the cache's local `main` is intentionally stale; new branch
  contains the latest `origin/main` commit.
- [ ] Launch from a local-path pick; behavior unchanged.

### Checkpoints
- `[CHECKPOINT:human-verify]` after Phase 3+4 land: run the TUI, star a remote repo,
  create a session, confirm branch base. Resume on "approved".

---

## Phase 4: Star toggles enforce remote-or-refuse
<!-- wave: 2 | depends_on: [1] | files: [crates/ainb-core/src/app/events.rs, crates/ainb-core/src/components/new_session/pick_repo.rs] -->

### Overview
Both star entry points must store a remote indicator or refuse with a notification.

### Changes Required

#### `crates/ainb-core/src/app/events.rs` — `StarSelectedWorkspace` (3266-3410)
- Replace the inline remote-resolution + three `LocalPath` fallbacks with the shared
  `favorite_from_local_repo(alias, &workspace.path)`:
  - `Ok(fav)` → toggle as today (dedupe on `fav.source`), success notification.
  - `Err(NoRemote | NotAGitRepo | Unparseable)` → **do not add**; call
    `state.add_error_notification("★ Can't favorite '<name>': no git remote (origin) found")`.
- Remove the local-path dedupe leg (`f.source == local_path_str`) since stars are never
  local now (keep a one-release tolerance: still allow *removing* a pre-existing local
  star by alias).

#### `crates/ainb-core/src/components/new_session/pick_repo.rs`
- `PickRepoOutcome`: add `Notice { message: String, is_error: bool }`.
- `toggle_favorite`: when the row source is `RepoSource::LocalPath(p)`, call
  `favorite_from_local_repo(row.id, &p)`; on `Err` return a refusal signal (don't persist);
  on `Ok` store the derived remote favorite. Remote sources store as today. Return an enum
  so `handle_key`'s `^F` branch can map it to `PickRepoOutcome::Notice` (error on refusal,
  info on add/remove) instead of always `Stay`.
- `handle_key` `^F` branch (`pick_repo.rs:537-545`): on refusal, return
  `Notice { is_error: true, .. }` and skip `rebuild_rows`; otherwise rebuild + return
  `Notice { is_error: false, .. }` (or `Stay`).

#### `crates/ainb-core/src/app/events.rs` — `handle_new_session_keys` (1321-1410)
- In the `match outcome`, add `PickRepoOutcome::Notice { message, is_error }` →
  `if is_error { state.add_error_notification(message) } else { state.add_info_notification(message) }`,
  then `None` (stay on picker).

### Success Criteria
#### Automated Verification
- [ ] `cargo build -p ainb-core` succeeds.
- [ ] `cargo test -p ainb-core pick_repo` passes (existing tests + new outcome test).
- [ ] `cargo clippy -p ainb-core -- -D warnings` clean.

#### Manual Verification
- [ ] Star a repo with no origin from the home list → red notification, nothing saved.
- [ ] Star a local repo *with* origin → saved as remote (`favorites.yaml` shows
  `github_shorthand`/`https_url`/`ssh_url`).
- [ ] `^F` on a `📁` local row in the picker → same behavior (refuse / derive-remote).

---

## Phase 5: Startup migration hook + integration tests
<!-- wave: 3 | depends_on: [1, 3, 4] | files: [crates/ainb-core/src/main.rs, crates/ainb-core/tests/star_remote_base.rs] -->

### Overview
Run the migration once on TUI startup and notify; add end-to-end coverage for the
stale-local-main scenario.

### Changes Required

#### `crates/ainb-core/src/main.rs` — TUI bootstrap (~137, after `App::new()`)
- Only in the `Some(("tui", _)) | None` arm (not CLI subcommands, not tests):
  `let mut store = FavoritesStore::load(); let report = store.migrate_local_to_remote();
  if !report.is_empty() { store.save()?; /* push notifications into app_state */ }`.
- Push one info notification summarizing `migrated.len()` rewritten, and one error
  notification listing `dropped` aliases ("removed N local-only stars without a remote:
  ...").

#### `crates/ainb-core/tests/star_remote_base.rs` (new)
- **stale-local-main**: `git init` source (`main`), commit A; clone into a temp "cache";
  add commit B to source and `git -C cache fetch`; leave cache local `main` at A; call
  `create_worktree_off_remote_default(cache, wt, "agents/test", source)`; assert the
  worktree contains commit B (i.e., based on `origin/main`, not stale local `main`).
- **master-default**: source default `master`; assert worktree base is `origin/master`.
- **migration**: write a `favorites.yaml` with a `LocalPath` entry pointing at a git repo
  with origin → `migrate_local_to_remote` rewrites it; a `LocalPath` to a non-repo → drop.

### Success Criteria
#### Automated Verification
- [ ] `cargo build -p ainb-core` succeeds.
- [ ] `cargo test -p ainb-core --test star_remote_base` passes.
- [ ] `cargo test -p ainb-core` (full) passes.
- [ ] `cargo clippy -p ainb-core -- -D warnings` clean.
- [ ] `cargo fmt --check` clean.

#### Manual Verification
- [ ] Start TUI with a pre-existing local-path star (with origin) in `favorites.yaml` →
  notification "migrated 1 star to remote", file rewritten.
- [ ] Start TUI with a local-path star whose path is not a repo → notification "removed",
  entry gone.

### Checkpoints
- `[CHECKPOINT:human-verify]` end-to-end smoke in the running TUI. Resume on "approved".

---

## Dependency Analysis
```
Wave 1: Phase 1 (favorites_store.rs)        ─┐
        Phase 2 (remote_repo_manager.rs)    ─┘ parallel, no file overlap
Wave 2: Phase 3 (state.rs)        depends_on Phase 2 ─┐
        Phase 4 (events.rs,pick_repo.rs) depends_on Phase 1 ─┘ parallel, no file overlap
Wave 3: Phase 5 (main.rs, tests/) depends_on Phase 1,3,4
```
No file appears in two phases of the same wave.

## Testing Strategy
- **Behavioral/integration first** (repo philosophy): the `tests/star_remote_base.rs`
  fixtures drive real `git` against temp repos — they prove the *outcome* (which commit the
  new branch is based on), not internal wiring.
- Module unit tests only where cheap and isolating (favorite classification, migration
  no-op idempotency, `Notice` outcome plumbing).
- Existing `pick_repo` tests must stay green (the `Notice` variant is additive).

## Migration Notes — does Stevie keep his existing stars?
- **Remote stars (github_shorthand / https_url / ssh_url): keep working as-is.** No action.
- **Local-path stars WITH an `origin` remote: auto-migrated on next TUI launch** — rewritten
  to the remote indicator, alias/stats/tags preserved. No re-star needed.
- **Local-path stars WITHOUT an origin (or path no longer a git repo): dropped** on next
  launch with a notification; these must be **re-created** once the repo has a remote.
- Net: **most existing stars survive automatically; only originless local stars need
  re-starring.**

## Risks
- **Cache staleness**: mitigated by the explicit `git fetch origin --prune` in
  `create_worktree_off_remote_default` (defensive on top of `clone_repo`'s fetch).
- **Odd/detached `origin/HEAD`**: `get_default_branch_name` returns `None` → fall back to
  `origin/main`→`origin/master` probe → error with a clear message rather than a wrong base.
- **Monorepo git scope**: star derivation opens the repo at the workspace/source path; for
  the monorepo root that resolves the toolkit's own origin — acceptable and matches today's
  `StarSelectedWorkspace` intent.
- **Notification plumbing from pick_repo**: solved by the additive `PickRepoOutcome::Notice`
  variant; no `AppState` reach-in from the pure component.
- **Two toggle paths drift**: both now call the single `favorite_from_local_repo` helper, so
  the rule can't diverge.

## References
- Diagnosis (this session): star = source-only pointer; base resolved by
  `get_default_branch` local-ref ladder.
- Locked decisions: refuse-if-no-origin; base = `origin/HEAD` fresh; auto-migrate on load;
  star-launched only.
- Key anchors: `favorites_store.rs`, `pick_repo.rs:646`, `events.rs:3266`, `state.rs:5708`,
  `state.rs:6352`, `worktree_manager.rs:407`, `remote_repo_manager.rs:152,426`,
  `repository.rs:83`.
