---
title: "CI / CD"
---

Four GitHub Actions workflows live under `.github/workflows/`. Each is path-filtered so it only runs when the area it covers changes.

## `ci.yml` — Rust CI

Runs on push and PR to `main` / `v2` when `ainb-tui/**`, `plugins/ainb-hooks/**`, `Cargo.*`, or the workflow itself changes. Jobs:

- **Rustfmt** — `cargo fmt --all --check` (Ubuntu).
- **Test** — `cargo nextest run --lib --all-features --no-fail-fast` on a `ubuntu-latest` + `macos-latest` matrix, skipping a small set of environment-dependent tests (`docker::`, a Claude provider-install test, and an inclusive-range usage test).
- **ainb-hooks** — daemon + plugin integration on both OSes: parses `plugins/ainb-hooks/hooks/notify.sh`, runs `ainb-plugin-notifyd` lib tests and the `end_to_end` (hook → socket → SQLite) test, plus the `tripwire_inbox_opens_and_renders` tripwire against the `ainb` binary. Installs `tmux` + `jq` on the Linux runner.
- **Cargo Machete** — `bnjbvr/cargo-machete` unused-dependency scan against `ainb-tui/Cargo.toml`.

## `release.yml` — tagged release + Homebrew tap

Manual `workflow_dispatch` (inputs: `version`, `prerelease`). Jobs run in sequence:

- **Prepare Release** — validates the semver `version`, generates changelog notes from conventional commits, bumps `ainb-tui/Cargo.toml`, updates `CHANGELOG.md`, commits, and creates + pushes the `v<version>` tag.
- **Build** — cross-platform release binaries on a matrix of `aarch64-apple-darwin` (macOS) and `x86_64-unknown-linux-gnu` (Linux), packaged as `.tar.gz` + `.sha256`.
- **Create Release** — `softprops/action-gh-release` publishes a GitHub Release with the artifacts and install instructions.
- **Update Homebrew Tap** — regenerates `Formula/ainb.rb` in `stevengonsalvez/homebrew-agents-in-a-box` with the new version and per-platform SHA256s, then commits and pushes.

## `toolkit-validation.yml` — toolkit structure + install checks

Runs on push and PR touching `toolkit/**`. Jobs:

- **Validate Packages Structure** — asserts `packages/{agents,skills,workflows,utilities}` exist and enforces minimum counts (agents ≥ 30, skills ≥ 60).
- **Test Tool Installations** — runs `bootstrap.js` into a fake `$HOME` for a `claude-code-4.5` / `codex` / `gemini` matrix, asserting the install dir, a minimum file count (≥ 100), a `skills/` dir, and an `agents/` dir for `claude-code-4.5`.
- **Check Template Substitution** — installs `claude-code-4.5` and greps for unsubstituted `{{TOOL_DIR}}` / `{{HOME_TOOL_DIR}}` placeholders.
- **Verify claude-code-4.5 is thin layer** — asserts `toolkit/claude-code-4.5` carries only the thin-layer files (no `agents`/`commands`/`skills`/etc. dirs).

## `deploy-pages.yml` — docs site to GitHub Pages

Runs on push to `main` touching `website/site/**`, `docs/**`, or the workflow itself (also `workflow_dispatch`). Builds the Astro + Starlight site under `website/site` with Node 22 and `npm ci` + `npm run build`, then deploys to GitHub Pages (`https://stevengonsalvez.github.io/agents-in-a-box/`). Uses a `pages` concurrency group so an in-flight deploy finishes while queued runs are cancelled.

## See also

- [Docs hub](../README.md)
