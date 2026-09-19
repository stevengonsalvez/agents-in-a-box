---
title: "Release process"
---

How a release is cut, tagged, built, and shipped.

## What this page will contain

- Version bumping (Cargo.toml + package.json)
- CHANGELOG.md update
- Tag + push
- Release workflow run
- Homebrew tap auto-update
- Verifying the release

## One version per release

The release workflow's `version` input names the tag, the CLI, the daemon,
the Fleet app and the desktop app. The prepare job bumps `ainb-tui/Cargo.toml`
(the workspace version every member crate inherits) and
`ainb-tui/crates/ainb-desktop/Cargo.toml` (the desktop is its own excluded
workspace, and the bundler reads its version from there; `tauri.conf.json`
carries none), regenerates both lockfiles, commits the bump on
`chore/release-v<version>`, and tags that commit. Every job downstream checks
out the tag.

## Desktop distribution

The `desktop` job of `.github/workflows/release.yml` bundles the shell with its
`ainb-hangar-daemon` sidecar for three targets, each on the runner the CLI's
own archive for that target is built on:

| Target | Runner | Bundle | Signing |
|---|---|---|---|
| `aarch64-apple-darwin` | `macos-latest` | `ainb-desktop-<version>-aarch64-apple-darwin.dmg` | ad-hoc, or Developer ID plus notarisation when `desktop_signed` is set |
| `x86_64-apple-darwin` | `macos-latest`, cross-compiled | `ainb-desktop-<version>-x86_64-apple-darwin.dmg` | same |
| `x86_64-unknown-linux-gnu` | `ubuntu-22.04` | `ainb-desktop-<version>-x86_64-unknown-linux-gnu.AppImage` and `.deb` | none |

Per-arch on macOS rather than universal: the sidecar is a target-triple-named
external binary that `cargo xtask stage-desktop-sidecar --release --target
<triple>` stages one at a time. Linux builds on the oldest supported image so
the AppImage links the oldest glibc. Linux arm64 is not built yet.

Before anything is bundled the job runs the staged sidecar's `--version` and
fails unless it reports the release version. The cross-compiled x64 sidecar
can run on the arm64 runner only under Rosetta; without it that one run is
skipped, the job summary says so, the binary's architecture is checked
instead, and the version inside the bundle is still asserted by the desktop
workflow's smoke on a matching host. After bundling the job reads the
bundle rather than launching it (a hosted runner is not a GUI host): the
`.app`'s `Info.plist` names the release version and the identifier
`dev.agentsinabox.desktop`, the sidecar sits at `Contents/MacOS/ainb-hangar-daemon`,
`codesign -v --strict --deep` passes, and the `.deb` carries both binaries under
`usr/bin`.

The bundler is `tauri-cli`, pinned to one exact version in the workflow
(`TAURI_CLI_VERSION`), invoked as `cargo tauri build --target <triple> --features
bundled --bundles <dmg | appimage,deb> -- --locked` from
`ainb-tui/crates/ainb-desktop`. The same command runs on a laptop.

### The signed manifest

The release job writes `release-manifest.json`, signs it with
`AINB_RELEASE_SIGNING_KEY` and attaches `release-manifest.sig`. The desktop
bundles are listed under the manifest's top-level `desktop` key, one entry per
bundle, each with `target`, `format` (`dmg`, `appimage` or `deb`, which is what
tells the two Linux entries apart), `archive`, `sha256` and a `signed` flag
that is true only when the bundle was signed with the Developer ID and
notarised. They are never listed under `assets[]`: a shipped CLI matches
`assets[]` on target alone and takes the first hit, so a `.dmg` there would be
installed as the `ainb` binary; `ainb-core/tests/update_domain.rs` pins that
with the 1.28.2 struct shape. The job reads the manifest back before signing
it and fails if any `assets[]` entry is not an `ainb-<version>-` archive or any
`desktop` entry is not an `ainb-desktop-<version>-` bundle with a known
format.

The CLI release never waits on the desktop legs: a bundle leg that fails
leaves its entries out of the `desktop` key, the release body names the
missing bundles, and the CLI archives, the Fleet app and the tap publish as
before. A pushed tag is never left without a release behind it. Two dispatches
of one version share a concurrency group keyed on the version, so the second
waits for the first.

The desktop updater reads the `dmg` and `appimage` entries; it never replaces a
package-managed install, so the `deb` entry is listed for completeness and
checksums only.

### Signing and notarisation

The macOS bundles are ad-hoc signed unless the workflow is run with
`desktop_signed` set: `bundle.macOS.signingIdentity` in `tauri.conf.json` is
`-`, the ad-hoc identity, because the bundler signs nothing at all when it is
given no identity, and an unsigned binary does not launch on Apple Silicon.
The unsigned bundle step carries no Apple variable in its environment at all,
since the bundler reads a defined-but-empty `APPLE_CERTIFICATE` as a
certificate to import. With it set, the job requires all six Apple secrets
below, passes them to the bundler, which signs with the Developer ID, submits
for notarisation and staples, and then asserts `spctl --assess --type execute`
accepts the app. Without it, the job records "ad-hoc signed, not notarised" in
its summary and the release body carries the Gatekeeper sentence the Fleet app
carries: first launch can require right-click Open, or Open Anyway in System
Settings > Privacy & Security.

The secrets are never read by an unsigned run: the bundler step receives them
only when `desktop_signed` is true.

## Fleet macOS distribution

Fleet shares the stable CLI release version. The release workflow passes its
`version` input into both `MARKETING_VERSION` and `CURRENT_PROJECT_VERSION`, so
the Git tag, cask, app bundle, and Sparkle appcast identify one release.
Prerelease tags do not ship Fleet, because the app's stable update feed must not
offer a prerelease to ordinary users.

Stable releases stop before tagging if a Sparkle update secret is absent. They
archive an ad-hoc-signed universal app, create an EdDSA-signed Sparkle appcast,
attach both to the GitHub release, and write `Casks/ainb-fleet.rb` into
`stevengonsalvez/homebrew-agents-in-a-box`.

Fleet is distributed only through the custom tap, not the Mac App Store or the
official Homebrew cask repository. `brew install --cask ainb-fleet` installs the
app, but macOS can require a first-launch Gatekeeper override because the app is
unsigned. Right-click the app and choose Open, or choose Open Anyway in System
Settings > Privacy & Security.

## Required repository secrets

| Secret | Purpose | Supplied by |
|---|---|---|
| `AINB_RELEASE_SIGNING_KEY` | Ed25519 private key (PEM) that signs `release-manifest.json`; the public half is pinned in `ainb-app/src/cli/update.rs`. | exists |
| `HOMEBREW_TAP_TOKEN` | Writes the formula and the Fleet cask into the tap. | exists |
| `SPARKLE_PUBLIC_ED_KEY` | Base64 Ed25519 public key embedded in Fleet `Info.plist`. | exists |
| `SPARKLE_PRIVATE_ED_KEY` | Matching Ed25519 private key used only to sign appcast enclosures. | exists |
| `APPLE_CERTIFICATE` | Base64 `.p12` of the Developer ID Application certificate, read by the Tauri bundler. | the human, #530 |
| `APPLE_CERTIFICATE_PASSWORD` | Password of that `.p12`. | the human, #530 |
| `APPLE_SIGNING_IDENTITY` | The identity name the bundler signs with, as `security find-identity` prints it. | the human, #530 |
| `APPLE_ID` | The Apple ID the bundler submits notarisation with. | the human, #530 |
| `APPLE_PASSWORD` | An app-specific password for that Apple ID. | the human, #530 |
| `APPLE_TEAM_ID` | The team the certificate belongs to. | the human, #530 |

The six Apple names are the variables the pinned Tauri CLI reads; their values
do not exist in this repository yet (#530). Nothing in the workflow signs with
an identity that is not this product's. Keep every private key in Bitwarden
and the matching GitHub repository secret. Never commit one.

## The release-branch rehearsal

The desktop programme's gate "release-branch human-driver run" is a human step.
It runs on a prerelease the workflow cut, on a macOS machine that can record
the screen, and its recording is linked from the programme row. It has two
halves: the install half (steps 1 to 3 and 7) proves the release matrix and
runs as soon as the matrix ships; the update half (steps 4 to 6) needs the
desktop updater and runs when it ships.

1. Cut the first rehearsal prerelease. In the repository, Actions, Release, run
   with `version` set to the next version with an `-rc1` suffix and
   `prerelease` checked. The workflow tags `v<X>-rc1` on
   `chore/release-v<X>-rc1`, which is the release branch the gate's name refers
   to. Prereleases skip the Fleet app and the tap.
2. Download the macOS `.dmg` for this machine's architecture from the release
   page. Open it, drag the app to Applications, eject. Launch it. When the
   bundle is ad-hoc signed, macOS refuses the first launch; use right-click,
   Open, or Open Anyway in System Settings, Privacy and Security.
3. In the window: the sidebar lists the tmux sessions the machine has, the
   sidecar banner reads connected, and the daemons panel in settings shows the
   hangar daemon with the release version and protocol range. Record whether
   the daemon was spawned by this app or attached to one an installed `ainb`
   already ran, and which version that one reports.
   Until the settings page carries an updates section, the updater's surface
   is the application menu on macOS (Check for Updates, Install Update and
   Restart, Roll Back Update, Remove Previous Version) and the channel is the
   file `desktop-updater.json` in the hangar home (`~/.agents-in-a-box` unless
   `AINB_HANGAR_HOME` says otherwise): `{"channel":"stable"}`,
   `{"channel":"prerelease","tag":"v<X>-rc1"}` or `{"channel":"off"}`, read
   at each check. The channel and the tag are set in that file or from a
   terminal only; the window can check, install, roll back and remove the
   previous version, never choose where updates come from. A file that does
   not parse declines every check with the reason until it is fixed. Every
   outcome arrives as a toast in the window, and an install shows its phase
   (downloading with progress, verifying, installing) while it runs. The
   previous version stays beside the app as the one rollback slot until the
   next installed update replaces it or Remove Previous Version is chosen.
4. Set the update channel to `prerelease` with the tag `v<X>-rc1` (the file
   above, or the settings section once it exists). Check for Updates. It
   reports current.
5. Cut `-rc2` the same way. Set the tag to `v<X>-rc2`. Check for Updates. It
   reports `<X>-rc2` available. Install Update and Restart. The window
   relaunches with no Gatekeeper prompt, because the updater leaves no
   quarantine attribute on what it downloads; if a prompt appears anyway, use
   the step 2 override, continue, and record it as a finding. About shows
   `-rc2`; the daemons panel shows the sidecar at `-rc2` when this app spawned
   it, or the older attached daemon marked older than this bundle with the
   banner naming the stop verb when it did not; run that verb in a terminal,
   click retry in the banner, and confirm the panel shows `-rc2`.
6. Roll Back Update from the menu. The window relaunches as `-rc1`, again with
   no prompt expected. The daemons panel shows the `-rc2` daemon still serving,
   marked newer than this app, and the update check reports `-rc2` available
   again. Run the stop verb the banner names, click retry: the panel shows an
   `-rc1` daemon and, if the `-rc2` daemon migrated the store forward, the red
   stale-daemon banner, which is the expected reading and is recorded as such.
7. Walk the screen script: open a session's terminal tab, create a session,
   open the board, answer an attention card, open the review tab, open
   settings. Recorded end to end; the recording is reviewed and linked from the
   programme row.
8. On a Linux desktop when one is at hand: run the AppImage, confirm the
   sidebar and the daemons panel as in step 3, and one update from `-rc1` to
   `-rc2`. When none is at hand, record "Linux leg: bundle smoke only".

## See also

- [Docs hub](/readme)
