//! Release discovery and self-update state.

use std::path::{Path, PathBuf};
use std::process::Command;

use ainb_plugin_notifyd::osnotify::Transport;
use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cli::OutputFormat;

/// The release host, the one source [`RELEASE_DOWNLOAD_ROOT`] and every
/// tagged (prerelease) root are formed from.
macro_rules! release_host {
    () => {
        "https://github.com/stevengonsalvez/agents-in-a-box"
    };
}

/// Where the `stable` channel fetches `release-manifest.json` and its
/// signature from. The one compiled-in root; a verified manifest's
/// [`ReleaseManifest::next_root`] can move a client off it.
pub const RELEASE_DOWNLOAD_ROOT: &str = concat!(release_host!(), "/releases/latest/download");

/// The release host [`RELEASE_DOWNLOAD_ROOT`] lives under, `https://` and
/// no trailing slash.
#[must_use]
pub const fn release_host() -> &'static str {
    release_host!()
}

/// `release-manifest.json` is a few kilobytes; anything past this is not it.
const MANIFEST_CAP: u64 = 256 * 1024;
/// A base64 Ed25519 signature is 88 bytes.
const SIGNATURE_CAP: u64 = 4 * 1024;
/// The largest bundle or archive a release may ship.
pub const DOWNLOAD_CAP: u64 = 512 * 1024 * 1024;
/// Redirects followed before a fetch gives up.
const REDIRECT_HOPS: usize = 3;
/// Hosts GitHub serves release assets from after a redirect off
/// [`release_host`].
const GITHUB_ASSET_HOSTS: [&str; 3] = [
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "github-releases.githubusercontent.com",
];
const RELEASE_SIGNING_PUBLIC_KEY_B64: &str = "2diG6eoKmUWKOk3XULwefjwKb5IIYTZA4xmNNA8Z6uk=";
const LAUNCHD_LABEL: &str = "com.agentsinabox.release-check";
const SYSTEMD_STEM: &str = "com.agentsinabox.release-check";

/// Signed release metadata fetched from the current stable GitHub release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseManifest {
    /// Stable semantic version without a leading `v`.
    pub version: String,
    /// Immutable archive metadata for each supported target: the CLI archives
    /// and nothing else. A shipped CLI matches this list on `target` alone and
    /// takes the first hit, which is why the desktop bundles live under
    /// [`Self::desktop`] and never here.
    #[serde(default)]
    pub assets: Vec<ReleaseAsset>,
    /// The desktop bundles, one per bundle, keyed by target and format. A CLI
    /// built before the key existed ignores it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub desktop: Vec<DesktopBundle>,
    /// A release root to fetch the NEXT manifest from, for a repository move:
    /// honoured only from a manifest that verified under the pinned key, and
    /// only as an `https://` root with a host (see [`validate_next_root`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_root: Option<String>,
}

/// One signed release archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseAsset {
    /// Rust target triple for the archive.
    pub target: String,
    /// Release asset file name.
    pub archive: String,
    /// SHA-256 of the archive, lowercase hexadecimal.
    pub sha256: String,
}

/// One desktop bundle: a `.dmg`, an AppImage or a `.deb`, for one target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopBundle {
    /// Rust target triple the bundle was built for.
    pub target: String,
    /// `dmg`, `appimage` or `deb`; what tells two bundles for one target apart.
    pub format: String,
    /// Release asset file name.
    pub archive: String,
    /// SHA-256 of the bundle file, lowercase hexadecimal.
    pub sha256: String,
    /// Whether the bundle was signed with a Developer ID and notarised, as
    /// opposed to ad-hoc signed. Informational: the trust root is this
    /// manifest's signature plus the checksum.
    #[serde(default)]
    pub signed: bool,
}

/// The archive-name and checksum rules every entry meets, whichever key it
/// sits under.
fn validate_archive_and_checksum(archive: &str, sha256: &str) -> Result<()> {
    let archive_is_file_name =
        std::path::Path::new(archive).file_name().is_some_and(|name| name == archive);
    if !archive_is_file_name
        || !archive
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    {
        bail!("release archive name is unsafe: {archive}");
    }
    if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("release archive checksum is invalid for {archive}");
    }
    Ok(())
}

impl ReleaseAsset {
    fn validate(&self) -> Result<()> {
        // Only a CLI archive belongs under `assets[]`: a shipped CLI matches
        // this list on target alone and takes the first hit, so a desktop
        // bundle here would be installed as the `ainb` binary. Refused at
        // verification, whichever key signed the manifest.
        if !self.archive.starts_with("ainb-") || !self.archive.ends_with(".tar.gz") {
            bail!(
                "release asset {} is not a CLI archive (ainb-<version>-<target>.tar.gz)",
                self.archive
            );
        }
        validate_archive_and_checksum(&self.archive, &self.sha256)
    }
}

impl DesktopBundle {
    fn validate(&self) -> Result<()> {
        if !matches!(self.format.as_str(), "dmg" | "appimage" | "deb") {
            bail!("desktop bundle format is unknown: {}", self.format);
        }
        validate_archive_and_checksum(&self.archive, &self.sha256)
    }
}

/// Whether `root` may replace the compiled-in release root: `https://`, a
/// host, an optional path, and no query, fragment, whitespace or non-ASCII.
///
/// # Errors
///
/// Names the first rule the value breaks.
pub fn validate_next_root(root: &str) -> Result<()> {
    let rest = root
        .strip_prefix("https://")
        .ok_or_else(|| anyhow::anyhow!("next_root must start with https://"))?;
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() {
        bail!("next_root has no host");
    }
    if !root.is_ascii() || root.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
        bail!("next_root carries whitespace or non-ASCII");
    }
    if root.contains('?') || root.contains('#') {
        bail!("next_root must not carry a query or a fragment");
    }
    Ok(())
}

impl ReleaseManifest {
    /// Small constructor retained for integration tests and fixture builders.
    #[must_use]
    pub fn for_test(version: &str) -> Self {
        Self {
            version: version.to_string(),
            assets: Vec::new(),
            desktop: Vec::new(),
            next_root: None,
        }
    }

    /// The desktop bundle for `target` in `format`, when the release has one.
    #[must_use]
    pub fn desktop_bundle_for(&self, target: &str, format: &str) -> Option<&DesktopBundle> {
        self.desktop
            .iter()
            .find(|bundle| bundle.target == target && bundle.format == format)
    }

    /// The release version as semver, refusing a prerelease.
    fn stable_version(&self) -> Result<Version> {
        self.version_allowing(false)
    }

    /// The release version as semver. A prerelease is an error unless
    /// `allow_prerelease`, which only the desktop's `prerelease` channel sets.
    fn version_allowing(&self, allow_prerelease: bool) -> Result<Version> {
        let version = Version::parse(self.version.trim_start_matches('v'))
            .with_context(|| format!("invalid release version `{}`", self.version))?;
        if !allow_prerelease && !version.pre.is_empty() {
            bail!("prerelease `{version}` is not eligible for stable updates");
        }
        Ok(version)
    }

    fn asset_for_current_platform(&self) -> Result<&ReleaseAsset> {
        let target = current_target()?;
        self.assets
            .iter()
            .find(|asset| asset.target == target)
            .with_context(|| format!("release {} has no archive for {target}", self.version))
    }
}

/// Verify a detached base64 Ed25519 signature and decode its JSON manifest.
pub fn verify_manifest(bytes: &[u8], signature_b64: &str) -> Result<ReleaseManifest> {
    verify_manifest_with_key(bytes, signature_b64, RELEASE_SIGNING_PUBLIC_KEY_B64)
}

/// Test seam for verifying a manifest under an explicit encoded public key.
pub fn verify_manifest_with_key(
    bytes: &[u8],
    signature_b64: &str,
    public_key_b64: &str,
) -> Result<ReleaseManifest> {
    let key_bytes = STANDARD
        .decode(public_key_b64.trim())
        .context("decoding Ed25519 release public key")?;
    let key_bytes: [u8; 32] = key_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("release public key must be 32 bytes"))?;
    let key = VerifyingKey::from_bytes(&key_bytes).context("parsing Ed25519 release public key")?;
    let signature_bytes =
        STANDARD.decode(signature_b64.trim()).context("decoding release signature")?;
    let signature =
        Signature::from_slice(&signature_bytes).context("parsing Ed25519 release signature")?;
    key.verify(bytes, &signature)
        .context("release manifest signature does not match")?;
    let manifest: ReleaseManifest =
        serde_json::from_slice(bytes).context("decoding signed release manifest")?;
    // Any semver parses here; whether a prerelease is eligible is the
    // caller's channel rule, applied in `ReleaseState::from_manifest_with`.
    manifest.version_allowing(true)?;
    for asset in &manifest.assets {
        asset.validate()?;
    }
    for bundle in &manifest.desktop {
        bundle.validate()?;
    }
    if let Some(root) = &manifest.next_root {
        validate_next_root(root)?;
    }
    Ok(manifest)
}

/// Release availability relative to the running Ainb binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateAvailability {
    /// A strictly newer stable release is available.
    Available,
    /// The running release equals or exceeds the latest stable release.
    CurrentOrNewer,
}

/// Durable result of the last successful release check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseState {
    /// Epoch milliseconds of the successful check.
    pub checked_at_ms: i64,
    /// Latest stable version returned by the release source.
    pub latest_version: String,
    /// Availability relative to the running binary.
    pub availability: UpdateAvailability,
    /// Latest version when an update may safely be installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_version: Option<String>,
    /// The release root the next check starts from, when a verified manifest
    /// named one (`next_root`); absent means the compiled-in root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}

impl ReleaseState {
    /// Derive durable availability from the running version and release
    /// manifest, refusing a prerelease manifest.
    pub fn from_manifest(
        local_version: &str,
        manifest: &ReleaseManifest,
        checked_at_ms: i64,
    ) -> Result<Self> {
        Self::from_manifest_with(local_version, manifest, checked_at_ms, false)
    }

    /// [`Self::from_manifest`] with the prerelease rule as a parameter: the
    /// desktop's `prerelease` channel passes `true`, every other caller
    /// `false`. Ordering is semver's in both cases.
    pub fn from_manifest_with(
        local_version: &str,
        manifest: &ReleaseManifest,
        checked_at_ms: i64,
        allow_prerelease: bool,
    ) -> Result<Self> {
        let local = Version::parse(local_version.trim_start_matches('v'))
            .with_context(|| format!("invalid local version `{local_version}`"))?;
        let latest = manifest.version_allowing(allow_prerelease)?;
        let availability = if latest > local {
            UpdateAvailability::Available
        } else {
            UpdateAvailability::CurrentOrNewer
        };
        Ok(Self {
            checked_at_ms,
            latest_version: latest.to_string(),
            available_version: (availability == UpdateAvailability::Available)
                .then(|| latest.to_string()),
            availability,
            root: manifest.next_root.clone(),
        })
    }

    /// Persist this complete release-check result without exposing torn JSON to readers.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        crate::fleet::plumbing::atomic::write_atomic_json(path, self)
    }

    /// Read one previously persisted release-check result.
    pub fn load_from(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading update state {}", path.display()))?;
        let state: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing update state {}", path.display()))?;
        // The root is data on disk: validated on the way back in, as it was
        // when the manifest named it, so a rewritten file cannot redirect the
        // next check to plain HTTP or an odd URL.
        if let Some(root) = &state.root {
            validate_next_root(root)
                .map_err(|error| anyhow::anyhow!("update state {}: {error}", path.display()))?;
        }
        Ok(state)
    }
}

/// OS timer definition for the daily, short-lived release checker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateSchedule {
    interval_secs: u64,
}

impl UpdateSchedule {
    /// Daily release check cadence.
    #[must_use]
    pub const fn daily() -> Self {
        Self {
            interval_secs: 24 * 60 * 60,
        }
    }

    /// Render the macOS LaunchAgent job. The shell resolves `ainb` at each run.
    #[must_use]
    pub fn launchd_plist(self, ainb_bin: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.agentsinabox.release-check</string>
  <key>ProgramArguments</key><array>
    <string>/bin/sh</string><string>-c</string><string>exec {ainb_bin} update check --scheduled</string>
  </array>
  <key>StartInterval</key><integer>{}</integer>
  <key>RunAtLoad</key><true/>
  <key>EnvironmentVariables</key><dict><key>PATH</key><string>{}</string></dict>
</dict></plist>
"#,
            self.interval_secs,
            crate::fleet::unit_program::unit_path_env(),
        )
    }

    /// Render the Linux systemd user timer. Persistent catch-up handles sleep.
    #[must_use]
    pub fn systemd_timer(self) -> String {
        format!(
            "[Unit]\nDescription=ainb release check timer\n\n[Timer]\nOnUnitActiveSec={}\nPersistent=true\nUnit=com.agentsinabox.release-check.service\n\n[Install]\nWantedBy=timers.target\n",
            self.interval_secs
        )
    }

    /// Render the Linux systemd user oneshot service.
    #[must_use]
    pub fn systemd_service(self, ainb_bin: &str) -> String {
        format!(
            "[Unit]\nDescription=ainb release check\n\n[Service]\nType=oneshot\nEnvironment=\"PATH={}\"\nExecStart=/bin/sh -c 'exec {} update check --scheduled'\n",
            crate::fleet::unit_program::unit_path_env(),
            ainb_bin,
        )
    }
}

/// Dispatch `ainb update` and its release-check scheduler controls.
pub async fn execute(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    match matches.subcommand() {
        Some(("check", sub)) => check_command(sub.get_flag("scheduled"), format).await,
        Some(("status", _)) => status_command(format),
        Some(("schedule", sub)) => schedule_command(sub),
        None => apply_command(matches.get_flag("yes")).await,
        _ => unreachable!("clap constrains update subcommands"),
    }
}

async fn check_command(scheduled: bool, format: OutputFormat) -> Result<()> {
    let state = fetch_release_state().await?;
    state.save_to(&state_path()?)?;
    if scheduled {
        notify_once_for_available(&state)?;
    }
    render_state(&state, format);
    Ok(())
}

fn status_command(format: OutputFormat) -> Result<()> {
    let path = state_path()?;
    match ReleaseState::load_from(&path) {
        Ok(state) => render_state(&state, format),
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
        {
            match format {
                OutputFormat::Json => println!(
                    r#"{{"checked":false,"scheduler_enabled":{}}}"#,
                    schedule_is_enabled()
                ),
                _ => println!("No release check yet. Run `ainb update check`."),
            }
        }
        Err(e) => return Err(e),
    }
    Ok(())
}

fn schedule_command(matches: &clap::ArgMatches) -> Result<()> {
    match matches.subcommand() {
        Some(("enable", _)) => enable_schedule(),
        Some(("disable", _)) => disable_schedule(),
        Some(("status", _)) => {
            println!(
                "release checker: {}",
                if schedule_is_enabled() {
                    "enabled"
                } else {
                    "disabled"
                }
            );
            Ok(())
        }
        _ => unreachable!("clap constrains schedule subcommands"),
    }
}

async fn apply_command(yes: bool) -> Result<()> {
    let manifest = fetch_release_manifest().await?;
    let state = ReleaseState::from_manifest(
        env!("CARGO_PKG_VERSION"),
        &manifest,
        chrono::Utc::now().timestamp_millis(),
    )?;
    state.save_to(&state_path()?)?;
    if state.availability != UpdateAvailability::Available {
        println!("ainb {} is current.", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let version = state.available_version.as_deref().expect("available version");
    if !yes {
        bail!("ainb {version} is available. Re-run with `ainb update --yes` to install it.");
    }
    let owner = InstallOwner::detect()?;
    owner.apply(&manifest).await?;
    println!("ainb update started for {version}; restart ainb to use it.");
    Ok(())
}

async fn fetch_release_state() -> Result<ReleaseState> {
    let manifest = fetch_release_manifest().await?;
    ReleaseState::from_manifest(
        env!("CARGO_PKG_VERSION"),
        &manifest,
        chrono::Utc::now().timestamp_millis(),
    )
}

async fn fetch_release_manifest() -> Result<ReleaseManifest> {
    fetch_release_manifest_at(RELEASE_DOWNLOAD_ROOT).await
}

/// Fetch and verify the manifest under `root`, with the pinned key.
///
/// # Errors
///
/// A request failure, a bad signature, or a manifest that fails validation.
pub async fn fetch_release_manifest_at(root: &str) -> Result<ReleaseManifest> {
    let (bytes, signature) = fetch_manifest_bytes_at(root).await?;
    verify_manifest(&bytes, &signature)
}

/// Fetch `<root>/release-manifest.json` and `<root>/release-manifest.sig`,
/// unverified, so a caller with its own key can verify them. `root` must be
/// `https://`; redirects stay on its host (or GitHub's asset hosts) and the
/// bodies are capped.
///
/// # Errors
///
/// A non-`https://` root, a request failure, a refused redirect, a body over
/// its cap, or a non-success status.
pub async fn fetch_manifest_bytes_at(root: &str) -> Result<(Vec<u8>, String)> {
    Fetch::STRICT.manifest(root).await
}

/// Download `url` to `path`, streamed under [`DOWNLOAD_CAP`], hashed as it
/// streams; returns the lowercase sha256 hex of what was written. `progress`
/// is called with (bytes so far, declared total) as chunks land. On any
/// failure nothing is left at `path`.
///
/// # Errors
///
/// A non-`https://` URL, a request failure, a refused redirect, a body over
/// the cap, a non-success status, or a write failure.
pub async fn download_to(
    url: &str,
    path: &Path,
    progress: &mut (dyn FnMut(u64, Option<u64>) + Send),
) -> Result<String> {
    Fetch::STRICT.download(url, path, progress).await
}

/// [`fetch_manifest_bytes_at`] that also accepts `http://` to a loopback
/// host, for tests that stand up their own responder. Not in release builds.
#[cfg(any(test, feature = "test-support"))]
pub async fn fetch_manifest_bytes_at_plain_loopback(root: &str) -> Result<(Vec<u8>, String)> {
    Fetch::PLAIN_LOOPBACK.manifest(root).await
}

/// [`download_to`] that also accepts `http://` to a loopback host, for tests
/// that stand up their own responder. Not in release builds.
#[cfg(any(test, feature = "test-support"))]
pub async fn download_to_plain_loopback(
    url: &str,
    path: &Path,
    progress: &mut (dyn FnMut(u64, Option<u64>) + Send),
) -> Result<String> {
    Fetch::PLAIN_LOOPBACK.download(url, path, progress).await
}

/// The one HTTP policy for everything the updater fetches: `https://` only,
/// at most [`REDIRECT_HOPS`] redirects, each staying on the first request's
/// host or one of [`GITHUB_ASSET_HOSTS`], bodies capped.
#[derive(Clone, Copy)]
struct Fetch {
    /// Test seam: `http://` to `127.0.0.1`, `localhost` or `[::1]` passes
    /// the scheme rule. Never set outside `cfg(test)`/`test-support`.
    allow_plain_loopback: bool,
}

impl Fetch {
    const STRICT: Self = Self {
        allow_plain_loopback: false,
    };
    #[cfg(any(test, feature = "test-support"))]
    const PLAIN_LOOPBACK: Self = Self {
        allow_plain_loopback: true,
    };

    fn scheme_ok(self, url: &reqwest::Url) -> bool {
        url.scheme() == "https"
            || (self.allow_plain_loopback
                && url.scheme() == "http"
                && matches!(
                    url.host_str(),
                    Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
                ))
    }

    fn parse(self, url: &str, what: &str) -> Result<reqwest::Url> {
        let parsed = reqwest::Url::parse(url).with_context(|| format!("{what} URL `{url}`"))?;
        if !self.scheme_ok(&parsed) {
            bail!("{what} must use https:// (got `{url}`)");
        }
        Ok(parsed)
    }

    fn client(self, agent: &str, timeout: std::time::Duration) -> Result<reqwest::Client> {
        let policy = reqwest::redirect::Policy::custom(move |attempt| {
            let first = attempt.previous().first().map(|url| url.host_str().map(str::to_owned));
            let next = attempt.url();
            if attempt.previous().len() >= REDIRECT_HOPS {
                return attempt.error("too many redirects");
            }
            if !self.scheme_ok(next) {
                return attempt.error("redirect off https refused");
            }
            let same_host = first.flatten().as_deref() == next.host_str();
            let asset_host = next.host_str().is_some_and(|host| GITHUB_ASSET_HOSTS.contains(&host));
            if same_host || asset_host {
                attempt.follow()
            } else {
                attempt.error("cross-host redirect refused")
            }
        });
        reqwest::Client::builder()
            .user_agent(format!("ainb/{} {agent}", env!("CARGO_PKG_VERSION")))
            .timeout(timeout)
            .redirect(policy)
            .build()
            .context("building release client")
    }

    async fn manifest(self, root: &str) -> Result<(Vec<u8>, String)> {
        let root = root.trim_end_matches('/');
        let manifest_url = self.parse(&format!("{root}/release-manifest.json"), "release root")?;
        let signature_url = self.parse(&format!("{root}/release-manifest.sig"), "release root")?;
        let client = self.client("update-check", std::time::Duration::from_secs(15))?;
        let manifest = read_capped(
            client
                .get(manifest_url)
                .send()
                .await
                .map_err(chain("downloading signed release manifest"))?,
            MANIFEST_CAP,
            "release manifest",
        )
        .await?;
        let signature = read_capped(
            client
                .get(signature_url)
                .send()
                .await
                .map_err(chain("downloading release manifest signature"))?,
            SIGNATURE_CAP,
            "release manifest signature",
        )
        .await?;
        let signature =
            String::from_utf8(signature).context("release manifest signature is not UTF-8")?;
        Ok((manifest, signature))
    }

    async fn download(
        self,
        url: &str,
        path: &Path,
        progress: &mut (dyn FnMut(u64, Option<u64>) + Send),
    ) -> Result<String> {
        let parsed = self.parse(url, "download")?;
        let client = self.client("updater", std::time::Duration::from_secs(600))?;
        let mut response = client
            .get(parsed)
            .send()
            .await
            .map_err(chain(&format!("downloading {url}")))?
            .error_for_status()
            .with_context(|| format!("download request failed for {url}"))?;
        let total = response.content_length();
        if total.is_some_and(|length| length > DOWNLOAD_CAP) {
            bail!(
                "download is too large ({} bytes over a {DOWNLOAD_CAP} byte cap)",
                total.unwrap_or(0)
            );
        }
        let mut file =
            std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
        let written = async {
            let mut hasher = Sha256::new();
            let mut received: u64 = 0;
            while let Some(chunk) = response.chunk().await.context("reading download")? {
                received += chunk.len() as u64;
                if received > DOWNLOAD_CAP {
                    bail!("download is too large (past a {DOWNLOAD_CAP} byte cap)");
                }
                hasher.update(&chunk);
                std::io::Write::write_all(&mut file, &chunk)
                    .with_context(|| format!("writing {}", path.display()))?;
                progress(received, total);
            }
            std::io::Write::flush(&mut file)
                .with_context(|| format!("writing {}", path.display()))?;
            Ok(format!("{:x}", hasher.finalize()))
        }
        .await;
        drop(file);
        if written.is_err() {
            let _ = std::fs::remove_file(path);
        }
        written
    }
}

/// Render a reqwest error with its causes on one line, so a refused
/// redirect or a TLS failure names itself at the top of the message.
fn chain(what: &str) -> impl FnOnce(reqwest::Error) -> anyhow::Error + '_ {
    move |error| anyhow::anyhow!("{what}: {:#}", anyhow::Error::new(error))
}

/// Read a small body under `cap`, refusing a declared or streamed length
/// past it before buffering it.
async fn read_capped(response: reqwest::Response, cap: u64, what: &str) -> Result<Vec<u8>> {
    let mut response =
        response.error_for_status().with_context(|| format!("{what} request failed"))?;
    if response.content_length().is_some_and(|length| length > cap) {
        bail!("{what} is too large (over a {cap} byte cap)");
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.with_context(|| format!("reading {what}"))? {
        if body.len() as u64 + chunk.len() as u64 > cap {
            bail!("{what} is too large (over a {cap} byte cap)");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn state_path() -> Result<PathBuf> {
    Ok(crate::fleet::plumbing::paths::ainb_home()?.join("update-state.json"))
}

fn notified_version_path() -> Result<PathBuf> {
    Ok(crate::fleet::plumbing::paths::ainb_home()?.join("update-notified-version"))
}

fn notify_once_for_available(state: &ReleaseState) -> Result<()> {
    let Some(version) = state.available_version.as_deref() else {
        return Ok(());
    };
    let path = notified_version_path()?;
    if std::fs::read_to_string(&path).ok().as_deref().map(str::trim) == Some(version) {
        return Ok(());
    }
    ainb_plugin_notifyd::osnotify::NativeTransport.emit(
        "ainb update available",
        &format!("ainb {version} is ready. Run ainb update --yes, then restart."),
    );
    crate::fleet::plumbing::atomic::write_atomic(&path, version.as_bytes())
}

fn render_state(state: &ReleaseState, format: OutputFormat) {
    match format {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string(state).expect("serializable update state")
        ),
        _ => match state.availability {
            UpdateAvailability::Available => println!(
                "ainb {} available (running {})",
                state.available_version.as_deref().unwrap_or(&state.latest_version),
                env!("CARGO_PKG_VERSION")
            ),
            UpdateAvailability::CurrentOrNewer => {
                println!("ainb {} is current.", env!("CARGO_PKG_VERSION"))
            }
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallOwner {
    Homebrew,
    Cargo,
    Direct,
}

impl InstallOwner {
    /// Which installation owns the binary the user actually invoked.
    ///
    /// The RUNNING executable decides. This used to probe
    /// `brew list --versions ainb` first and answer `Homebrew` on success
    /// without consulting `exe` at all, so on a machine where Homebrew owns one
    /// copy and a second copy shadows it on `PATH`, `ainb update` upgraded
    /// Homebrew's, reported success, and left the binary the user ran
    /// untouched. That copy could never be updated by the updater however many
    /// times it was run.
    ///
    /// Not hypothetical: a `~/.local/bin/ainb` source build shadowed a Homebrew
    /// 1.24.0 and pinned the machine to 1.23.2, surfacing only when its
    /// embedded migrations were too old to open a database a newer build had
    /// already migrated forward.
    fn detect() -> Result<Self> {
        let exe = std::env::current_exe().context("resolving ainb executable")?;
        if let Some(owner) = classify_exe(&exe, dirs::home_dir().as_deref()) {
            return Ok(owner);
        }
        if Command::new("brew")
            .args(["list", "--versions", "ainb"])
            .output()
            .ok()
            .is_some_and(|out| out.status.success())
        {
            // Printed on stdout, beside the completion line `apply_command`
            // writes, NOT on stderr. A caller capturing only stdout would
            // otherwise read an unqualified success for an upgrade that did not
            // touch the running binary, which is the same silent
            // wrong-binary failure this function exists to remove.
            println!(
                "warning: upgrading the Homebrew ainb, but you are running {}, \
                 which is not it. That copy stays at its current version.",
                exe.display()
            );
            return Ok(Self::Homebrew);
        }
        bail!(
            "ainb installation is unmanaged; reinstall with the curl installer or use your package manager"
        )
    }

    async fn apply(self, manifest: &ReleaseManifest) -> Result<()> {
        if self == Self::Direct {
            return apply_direct_release(manifest).await;
        }
        if self == Self::Homebrew {
            let status =
                homebrew_update_command().status().context("refreshing Homebrew metadata")?;
            if !status.success() {
                bail!("Homebrew metadata refresh exited {status}");
            }
        }
        let version = manifest.stable_version()?.to_string();
        let mut command = match self {
            Self::Homebrew => homebrew_upgrade_command(),
            Self::Cargo => {
                let mut c = Command::new("cargo");
                c.args([
                    "install",
                    "--git",
                    "https://github.com/stevengonsalvez/agents-in-a-box",
                    "--tag",
                    &format!("v{version}"),
                    "--locked",
                    "--force",
                    "ainb",
                ]);
                c
            }
            Self::Direct => unreachable!("handled above"),
        };
        let status = command.status().context("running ainb installer")?;
        if status.success() {
            Ok(())
        } else {
            bail!("ainb update command exited {status}")
        }
    }
}

/// Refresh the local tap checkout before Homebrew compares formula versions.
///
/// `brew upgrade` can otherwise report an old installed formula as current
/// when its local tap checkout predates the signed release manifest that ainb
/// just verified.
fn homebrew_update_command() -> Command {
    let mut command = Command::new("brew");
    command.arg("update");
    command
}

fn homebrew_upgrade_command() -> Command {
    let mut command = Command::new("brew");
    command.args(["upgrade", "ainb"]);
    command
}

async fn apply_direct_release(manifest: &ReleaseManifest) -> Result<()> {
    let asset = manifest.asset_for_current_platform()?;
    let url = format!(
        "{}/releases/download/v{}/{}",
        release_host(),
        manifest.stable_version()?,
        asset.archive,
    );
    let temp = tempfile::tempdir().context("creating release staging directory")?;
    let archive = temp.path().join(&asset.archive);
    let actual_sha = download_to(&url, &archive, &mut |_, _| {}).await?;
    if !actual_sha.eq_ignore_ascii_case(asset.sha256.trim()) {
        bail!("release archive checksum mismatch for {}", asset.archive);
    }
    let unpack = Command::new("tar")
        .args(["-xzf"])
        .arg(&archive)
        .arg("-C")
        .arg(temp.path())
        .status()
        .context("extracting verified release archive")?;
    if !unpack.success() {
        bail!("extracting verified release archive exited {unpack}");
    }

    let staged_binary = temp.path().join("ainb");
    verify_candidate_binary(&staged_binary)?;
    let destination = std::env::current_exe().context("resolving installed ainb binary")?;
    let parent = destination.parent().context("resolving ainb install directory")?;
    let replacement = parent.join(".ainb-update-next");
    std::fs::copy(&staged_binary, &replacement)
        .with_context(|| format!("staging replacement at {}", replacement.display()))?;
    make_executable(&replacement)?;
    re_sign_macos_binary(&replacement)?;
    verify_candidate_binary(&replacement)?;
    let mut plugins = DirectPluginSwap::stage(temp.path(), parent)?;
    if let Some(swap) = plugins.as_mut() {
        swap.activate()?;
    }
    if let Err(error) = std::fs::rename(&replacement, &destination) {
        if let Some(swap) = plugins.as_mut() {
            swap.rollback().context("rolling back bundled plugins")?;
        }
        return Err(error).with_context(|| format!("replacing {}", destination.display()));
    }
    if let Some(swap) = plugins {
        swap.commit()?;
    }
    Ok(())
}

/// The target triple a release archive or bundle for this build is named by.
///
/// # Errors
///
/// A platform the release matrix does not build.
pub fn current_target() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        (os, arch) => bail!("no signed ainb release archive for {arch}-{os}"),
    }
}

fn verify_candidate_binary(path: &Path) -> Result<()> {
    if !path.is_file() {
        bail!("verified release archive did not contain ainb binary");
    }
    let output = Command::new(path)
        .arg("--version")
        .output()
        .with_context(|| format!("running candidate {}", path.display()))?;
    if output.status.success() {
        Ok(())
    } else {
        bail!("candidate ainb binary failed its version check")
    }
}

fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

fn re_sign_macos_binary(path: &Path) -> Result<()> {
    if cfg!(target_os = "macos") {
        let status = Command::new("codesign")
            .args(["--force", "--sign", "-"])
            .arg(path)
            .status()
            .context("ad-hoc signing replacement ainb binary")?;
        if !status.success() {
            bail!("codesign replacement binary exited {status}");
        }
    }
    Ok(())
}

struct DirectPluginSwap {
    current: PathBuf,
    next: PathBuf,
    backup: PathBuf,
}

impl DirectPluginSwap {
    fn stage(staging_root: &Path, install_dir: &Path) -> Result<Option<Self>> {
        let staged = staging_root.join("plugins");
        if !staged.is_dir() {
            return Ok(None);
        }
        let next = install_dir.join(".ainb-plugins-next");
        if next.exists() {
            std::fs::remove_dir_all(&next)
                .with_context(|| format!("clearing {}", next.display()))?;
        }
        let copy = Command::new("cp")
            .args(["-R"])
            .arg(&staged)
            .arg(&next)
            .status()
            .context("staging bundled plugins")?;
        if !copy.success() {
            bail!("staging bundled plugins exited {copy}");
        }
        Ok(Some(Self {
            current: install_dir.join("plugins"),
            next,
            backup: install_dir.join(".ainb-plugins-previous"),
        }))
    }

    fn activate(&mut self) -> Result<()> {
        if self.backup.exists() {
            std::fs::remove_dir_all(&self.backup)
                .with_context(|| format!("clearing {}", self.backup.display()))?;
        }
        if self.current.exists() {
            std::fs::rename(&self.current, &self.backup).context("backing up bundled plugins")?;
        }
        if let Err(error) = std::fs::rename(&self.next, &self.current) {
            if self.backup.exists() {
                let _ = std::fs::rename(&self.backup, &self.current);
            }
            return Err(error).context("activating bundled plugins");
        }
        Ok(())
    }

    fn rollback(&mut self) -> Result<()> {
        if self.current.exists() {
            std::fs::remove_dir_all(&self.current)
                .with_context(|| format!("removing replacement {}", self.current.display()))?;
        }
        if self.backup.exists() {
            std::fs::rename(&self.backup, &self.current).context("restoring bundled plugins")?;
        }
        Ok(())
    }

    fn commit(self) -> Result<()> {
        if self.backup.exists() {
            std::fs::remove_dir_all(&self.backup)
                .with_context(|| format!("removing previous {}", self.backup.display()))?;
        }
        Ok(())
    }
}

/// Which installation owns `exe`, by path alone. `None` when it matches nothing
/// known, which is the only case that may fall back to a probe.
///
/// Split out of [`InstallOwner::detect`] so the ORDER is testable without a
/// Homebrew on the machine: the regression this guards is a `Direct` exe being
/// answered `Homebrew` merely because Homebrew also had a copy somewhere.
///
/// Homebrew is recognised by a `Cellar` ancestor, not by asking
/// `brew --prefix ainb` where its binary is. The formula installs the real
/// executable at `<keg>/libexec/ainb` and writes `<keg>/bin/ainb` as a shell
/// wrapper that `exec`s it, so `current_exe` under Homebrew reports the libexec
/// path and never equals the prefix's `bin/ainb`. Comparing against that
/// wrapper made the Homebrew arm unreachable and gave every ordinary Homebrew
/// user a warning naming their own live binary as a stale shadow. The ancestor
/// walk is the same rule `ainb_plugin_notifyd::install::homebrew_launcher`
/// uses, and it costs no subprocess.
///
/// Checked BEFORE the direct paths on purpose: a brew-managed
/// `/usr/local/bin/ainb` is a symlink into the Cellar, and canonicalising it
/// reaches the keg, so answering `Direct` there would let
/// `apply_direct_release` replace a Homebrew-owned link with an unmanaged file.
fn classify_exe(exe: &Path, home: Option<&Path>) -> Option<InstallOwner> {
    let resolved = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    if resolved
        .ancestors()
        .any(|path| path.file_name().is_some_and(|name| name == "Cellar"))
    {
        return Some(InstallOwner::Homebrew);
    }
    if resolved.parent().is_some_and(|parent| parent.ends_with(".cargo/bin")) {
        return Some(InstallOwner::Cargo);
    }
    // `home` guarded rather than defaulted: an empty base yields the RELATIVE
    // `.local/bin/ainb`, which `canonicalize` resolves against the process
    // working directory, so an unrelated binary under the cwd could be
    // classified `Direct` and then overwritten.
    let mut direct = vec![PathBuf::from("/usr/local/bin/ainb")];
    if let Some(home) = home {
        direct.push(home.join(".local/bin/ainb"));
    }
    direct
        .iter()
        .any(|path| canonical_eq(path, &resolved))
        .then_some(InstallOwner::Direct)
}

/// Whether two paths name the same existing file.
///
/// Both sides must resolve. Comparing `canonicalize(..).ok()` instead made two
/// paths that BOTH fail to resolve compare equal, so an exe that does not exist
/// matched a `/usr/local/bin/ainb` that does not exist either and was reported
/// as a Direct install. `current_exe` always resolves, so this never misfired in
/// production, but "neither of these exists" is not "these are the same binary".
fn canonical_eq(left: &Path, right: &Path) -> bool {
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn launchd_path() -> Option<PathBuf> {
    dirs::home_dir()
        .map(|home| home.join("Library/LaunchAgents").join(format!("{LAUNCHD_LABEL}.plist")))
}

fn systemd_paths() -> Option<(PathBuf, PathBuf)> {
    dirs::home_dir().map(|home| {
        let dir = home.join(".config/systemd/user");
        (
            dir.join(format!("{SYSTEMD_STEM}.service")),
            dir.join(format!("{SYSTEMD_STEM}.timer")),
        )
    })
}

/// Whether the operating-system daily release checker is installed.
#[must_use]
pub fn schedule_is_enabled() -> bool {
    if cfg!(target_os = "macos") {
        launchd_path().is_some_and(|path| path.exists())
    } else {
        systemd_paths().is_some_and(|(_, timer)| timer.exists())
    }
}

/// Install the daily release checker if it is not already installed.
pub fn ensure_schedule() -> Result<()> {
    if schedule_is_enabled() {
        return Ok(());
    }
    let schedule = UpdateSchedule::daily();
    if cfg!(target_os = "macos") {
        let path = launchd_path().context("resolving LaunchAgents path")?;
        crate::fleet::plumbing::atomic::write_atomic(
            &path,
            schedule.launchd_plist("ainb").as_bytes(),
        )?;
        let _ = Command::new("launchctl").args(["unload", &path.display().to_string()]).output();
        let _ = Command::new("launchctl").args(["load", &path.display().to_string()]).output();
    } else {
        let (service, timer) = systemd_paths().context("resolving systemd user directory")?;
        crate::fleet::plumbing::atomic::write_atomic(
            &service,
            schedule.systemd_service("ainb").as_bytes(),
        )?;
        crate::fleet::plumbing::atomic::write_atomic(&timer, schedule.systemd_timer().as_bytes())?;
        let _ = Command::new("systemctl").args(["--user", "daemon-reload"]).output();
        let _ = Command::new("systemctl")
            .args([
                "--user",
                "enable",
                "--now",
                &format!("{SYSTEMD_STEM}.timer"),
            ])
            .output();
        let _ = Command::new("systemctl")
            .args(["--user", "start", &format!("{SYSTEMD_STEM}.service")])
            .output();
    }
    Ok(())
}

fn enable_schedule() -> Result<()> {
    ensure_schedule()?;
    println!("Daily ainb release check enabled.");
    Ok(())
}

/// Remove the daily release checker and its OS registration.
pub fn disable_schedule() -> Result<()> {
    if cfg!(target_os = "macos") {
        if let Some(path) = launchd_path().filter(|path| path.exists()) {
            let _ =
                Command::new("launchctl").args(["unload", &path.display().to_string()]).output();
            std::fs::remove_file(path).context("removing release-check LaunchAgent")?;
        }
    } else if let Some((service, timer)) = systemd_paths() {
        let _ = Command::new("systemctl")
            .args([
                "--user",
                "disable",
                "--now",
                &format!("{SYSTEMD_STEM}.timer"),
            ])
            .output();
        for path in [timer, service] {
            if path.exists() {
                std::fs::remove_file(&path)
                    .with_context(|| format!("removing {}", path.display()))?;
            }
        }
        let _ = Command::new("systemctl").args(["--user", "daemon-reload"]).output();
    }
    println!("Daily ainb release check disabled.");
    Ok(())
}

/// Read the last verified update result when one exists.
#[must_use]
pub fn cached_state() -> Option<ReleaseState> {
    state_path().ok().and_then(|path| ReleaseState::load_from(&path).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shadowing_direct_exe_is_not_claimed_by_homebrew() {
        // Real files throughout: `canonical_eq` requires both sides to resolve,
        // and the Cellar walk runs on the CANONICAL exe, so fake paths would
        // prove nothing.
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let local_bin = home.join(".local/bin");
        let cargo_bin = home.join(".cargo/bin");
        let elsewhere = tmp.path().join("elsewhere");
        // The real formula shape: the executable lives in `<keg>/libexec` and
        // `<keg>/bin/ainb` is a shell wrapper that execs it, so a Homebrew run
        // reports the LIBEXEC path. Modelling both as one path is what let the
        // first version of this test pass while the production arm was dead.
        let keg = tmp.path().join("Cellar/ainb/1.24.0");
        let brew_libexec = keg.join("libexec");
        let brew_bin = keg.join("bin");
        for dir in [&local_bin, &cargo_bin, &elsewhere, &brew_libexec, &brew_bin] {
            std::fs::create_dir_all(dir).expect("create dir");
            std::fs::write(dir.join("ainb"), b"binary").expect("write binary");
        }

        // The regression. Homebrew has a copy, but the running exe is the
        // ~/.local/bin one that shadows it on PATH. Answering Homebrew here is
        // what let `ainb update` upgrade a binary the user was not running,
        // report success, and leave the shadowing copy stale forever.
        assert_eq!(
            classify_exe(&local_bin.join("ainb"), Some(&home)),
            Some(InstallOwner::Direct),
            "a shadowing ~/.local/bin exe owns itself"
        );

        // The arm that was unreachable in production: the libexec binary, not
        // the wrapper, is what `current_exe` reports under Homebrew.
        assert_eq!(
            classify_exe(&brew_libexec.join("ainb"), Some(&home)),
            Some(InstallOwner::Homebrew),
            "the real Homebrew binary lives in libexec and must classify as Homebrew"
        );
        assert_eq!(
            classify_exe(&brew_bin.join("ainb"), Some(&home)),
            Some(InstallOwner::Homebrew),
            "the wrapper is Homebrew's too"
        );

        assert_eq!(
            classify_exe(&cargo_bin.join("ainb"), Some(&home)),
            Some(InstallOwner::Cargo),
            "a ~/.cargo/bin exe is Cargo's"
        );
        assert_eq!(
            classify_exe(&elsewhere.join("ainb"), Some(&home)),
            None,
            "a real binary in an unrecognised directory falls through to the probe"
        );

        // No home: the `~/.local/bin` candidate must not degrade to the
        // relative `.local/bin/ainb`, which canonicalises against the process
        // working directory.
        assert_eq!(
            classify_exe(&local_bin.join("ainb"), None),
            None,
            "without a home there is no ~/.local/bin candidate to match"
        );
        assert_eq!(
            classify_exe(&brew_libexec.join("ainb"), None),
            Some(InstallOwner::Homebrew),
            "Homebrew is decided by the Cellar ancestor, with or without a home"
        );
    }

    #[test]
    fn homebrew_update_refreshes_formula_metadata_before_upgrade() {
        let command = homebrew_update_command();
        assert_eq!(command.get_program(), "brew");
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["update"]);
    }

    #[test]
    fn homebrew_upgrade_targets_only_ainb() {
        let command = homebrew_upgrade_command();
        assert_eq!(command.get_program(), "brew");
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["upgrade", "ainb"]);
    }

    #[test]
    fn plugin_swap_rolls_back_when_binary_activation_fails() {
        let temp = tempfile::tempdir().unwrap();
        let staging = temp.path().join("staging");
        let install = temp.path().join("install");
        std::fs::create_dir_all(staging.join("plugins")).unwrap();
        std::fs::create_dir_all(install.join("plugins")).unwrap();
        std::fs::write(staging.join("plugins/new-plugin"), "new").unwrap();
        std::fs::write(install.join("plugins/old-plugin"), "old").unwrap();

        let mut swap = DirectPluginSwap::stage(&staging, &install).unwrap().unwrap();
        swap.activate().unwrap();
        assert_eq!(
            std::fs::read_to_string(install.join("plugins/new-plugin")).unwrap(),
            "new"
        );

        swap.rollback().unwrap();
        assert_eq!(
            std::fs::read_to_string(install.join("plugins/old-plugin")).unwrap(),
            "old"
        );
        assert!(!install.join("plugins/new-plugin").exists());
    }
}
