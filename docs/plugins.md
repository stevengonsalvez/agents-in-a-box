# ainb plugins

User-facing reference for the `ainb plugin` family of commands. If you're
authoring a plugin, jump to [docs/plugin-authoring.md](./plugin-authoring.md).

## What is a plugin?

A plugin is a self-contained capsule that adds a screen, command, sidebar
entry, statusline segment, or provider to ainb without recompiling the
host. Plugins are compiled to `wasm32-wasip1` and run inside a wasmi
sandbox: they only see the host capabilities they declare in their
manifest, and they cannot reach the network, filesystem, or subprocess
launcher unless you approve those caps at install time.

The first plugin shipped in-tree is **burndown**, which owns the
Analytics screen, the `ainb usage` CLI subcommand tree, and the budget
statusline segment. Removing it would have left ainb without an
Analytics screen — that's the point of Phase 5's tripwire.

## Where things live

ainb stores plugin state under `~/.agents-in-a-box/plugins/` (override
with `$AINB_HOME`):

```
~/.agents-in-a-box/plugins/
├── .lock                       # fs2 advisory lock — one install at a time
├── installed.toml              # lockfile of installed plugins + approved caps
├── marketplaces/
│   └── ainb-plugins/
│       └── marketplace.json    # registered marketplace catalogs
├── cache/
│   └── ainb-plugins/
│       └── burndown/
│           └── 0.1.0/
│               ├── plugin.toml
│               └── plugin.wasm
└── data/
    └── burndown/               # plugin-owned writable storage
```

The host loads any plugin under `cache/<marketplace>/<plugin>/<version>/`
on startup. `installed.toml` records the version and the capabilities
you approved — used by `ainb plugin update` to detect new requests.

## Commands

### Marketplaces

A marketplace is a JSON catalog (`marketplace.json`) that lists plugins
available for install. Catalogs ship inside repositories; you register
each marketplace once.

```bash
# Register a marketplace from a public repo (shallow git clone).
ainb plugin marketplace add https://github.com/stevengonsalvez/agents-in-a-box

# Register from a local directory or file URL (useful for development).
ainb plugin marketplace add file:///path/to/marketplace.json
ainb plugin marketplace add ./local-mkt/marketplace.json

# List registered marketplaces.
ainb plugin marketplace list
ainb plugin marketplace list --format=json

# Remove a marketplace (does not uninstall plugins already installed
# from it).
ainb plugin marketplace remove ainb-plugins
```

ainb reads the catalog from `<repo>/.ainb-plugin/marketplace.json` —
or, for compatibility with Claude Code-style plugins,
`<repo>/.claude-plugin/marketplace.json`. Both work.

### Search

```bash
# Substring match against plugin names across every registered marketplace.
ainb plugin search burn
ainb plugin search burn --format=json
```

### Install

```bash
# Install latest version found in any registered marketplace.
ainb plugin install burndown

# Pin to a version.
ainb plugin install burndown@0.1.0

# Disambiguate when more than one marketplace ships the plugin.
ainb plugin install ainb-plugins/burndown@0.1.0

# Skip the capability prompt (CI / scripted installs).
ainb plugin install burndown --yes
```

The installer:

1. Resolves the entry through your registered marketplaces.
2. Fetches `plugin.wasm` (and the `plugin.toml` manifest).
3. Validates the manifest against the host version requirement.
4. Prints the requested capabilities and asks you to confirm.
5. Writes the artifacts under `cache/<mkt>/<plugin>/<version>/`.
6. Records the install in `installed.toml`.

### List

```bash
ainb plugin list                  # human-readable
ainb plugin list --format=json    # machine-readable
```

### Update

```bash
ainb plugin update burndown
ainb plugin update burndown --yes
```

`update` re-resolves the latest version through whichever marketplace
the plugin was originally installed from. If the new version asks for
**any capability the lockfile doesn't already record as approved**, the
prompt fires again — even with `--yes` the lockfile still tracks every
approved capability. The previous version's cache directory is removed
on a successful update.

### Remove

```bash
ainb plugin remove burndown
ainb plugin remove burndown --yes    # also delete data/<plugin>/ without prompting
```

`remove` drops the cache directory and the lockfile entry. If
`data/<plugin>/` exists (the plugin's writable storage), ainb confirms
before deleting — `--yes` skips that confirmation.

## Capability model

Every plugin declares the host capabilities it needs in `plugin.toml`:

```toml
[capabilities]
read_sessions       = true     # ~/.claude/agents-in-a-box/sessions/**
read_claude_logs    = true     # ~/.claude/projects/**/*.jsonl
read_codex_logs     = false    # ~/.codex/logs/**/*.jsonl
write_plugin_data   = true     # data/<plugin>/ writable
event_bus           = false    # publish/subscribe across plugins
spawn_subprocess    = false    # exec child processes
network             = []       # explicit allowlist of host:port
filesystem          = []       # explicit allowlist of glob patterns
```

At install time ainb prints the truthy flags and the network /
filesystem allowlists, and waits for `y` (or `--yes`). The host will
trap any host-fn call the plugin makes against a capability you didn't
approve.

`update` only re-prompts when the new manifest requests something *not
already* in `capabilities_approved` for that plugin. Removed
capabilities don't trigger a prompt — they shrink the surface,
which is always safe.

## Configuration

### Disable plugins

`AINB_DISABLE_PLUGINS=1 ainb tui` boots the host with no plugins
loaded. Useful for bisecting plugin-induced regressions without having
to uninstall every plugin. The Analytics screen falls back to a
"plugin: rendering…" placeholder when its owner plugin isn't loaded.

### Override the install root

`AINB_HOME=/some/path ainb plugin …` redirects every cache /
marketplace / lockfile path under `/some/path/plugins/` instead of
`~/.agents-in-a-box/`. Used by tests and isolated CI environments.

### Override the plugin search root

`AINB_PLUGIN_ROOT=/path/to/dist/plugins ainb tui` makes the host load
plugins from a flat `<plugin-id>/plugin.{toml,wasm}` layout. Used by
`scripts/build-plugins.sh` for development.

## Cold-install verification

Phase 5 sign-off includes a `[CHECKPOINT:human-verify]` step that the
release manager runs on a fresh dev machine:

1. `git clone https://github.com/stevengonsalvez/agents-in-a-box`
2. `cd agents-in-a-box/ainb-tui`
3. `cargo build --workspace --release`
4. `./target/release/ainb plugin marketplace add ../`
5. `./target/release/ainb plugin install burndown`
6. `./target/release/ainb tui`
7. Press `A` — Analytics renders. Press `Esc` — back to home.
8. `./target/release/ainb plugin remove burndown` — Analytics screen
   gone next launch (graceful placeholder).

The automated tripwire (`cargo test --test tripwire`) runs the same
shape headlessly on every PR.

## Troubleshooting

* **`marketplace.json failed to parse`** — the catalog isn't valid
  JSON, or it's missing required fields. Check the schema in
  [docs/plugin-authoring.md](./plugin-authoring.md#marketplace-schema).
* **`no marketplaces registered`** — run `ainb plugin marketplace add`
  first.
* **`plugin 'X' is offered by multiple marketplaces`** — disambiguate
  with `<marketplace>/<plugin>` syntax.
* **`acquire install lock`** failures — another `ainb plugin` process
  is mid-install. The fs2 advisory lock under
  `<plugins-root>/.lock` serialises installs so the cache + lockfile
  never race; re-running once the other process exits resolves the
  block.
* **Capability prompt times out** — pipe input or pass `--yes`.
* **Analytics shows "plugin: rendering…" but never updates** — the
  burndown plugin failed to load. Run `RUST_LOG=info ainb tui` and
  look for `failed to load plugin` warnings.
