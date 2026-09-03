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

const RELEASE_DOWNLOAD_ROOT: &str =
    "https://github.com/stevengonsalvez/agents-in-a-box/releases/latest/download";
const RELEASE_SIGNING_PUBLIC_KEY_B64: &str = "2diG6eoKmUWKOk3XULwefjwKb5IIYTZA4xmNNA8Z6uk=";
const LAUNCHD_LABEL: &str = "com.agentsinabox.release-check";
const SYSTEMD_STEM: &str = "com.agentsinabox.release-check";

/// Signed release metadata fetched from the current stable GitHub release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseManifest {
    /// Stable semantic version without a leading `v`.
    pub version: String,
    /// Immutable archive metadata for each supported target.
    #[serde(default)]
    pub assets: Vec<ReleaseAsset>,
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

impl ReleaseAsset {
    fn validate(&self) -> Result<()> {
        let archive_is_file_name = std::path::Path::new(&self.archive)
            .file_name()
            .is_some_and(|name| name == self.archive.as_str());
        if !archive_is_file_name
            || !self
                .archive
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
        {
            bail!("release archive name is unsafe: {}", self.archive);
        }
        if self.sha256.len() != 64 || !self.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("release archive checksum is invalid for {}", self.archive);
        }
        Ok(())
    }
}

impl ReleaseManifest {
    /// Small constructor retained for integration tests and fixture builders.
    #[must_use]
    pub fn for_test(version: &str) -> Self {
        Self {
            version: version.to_string(),
            assets: Vec::new(),
        }
    }

    fn stable_version(&self) -> Result<Version> {
        let version = Version::parse(self.version.trim_start_matches('v'))
            .with_context(|| format!("invalid release version `{}`", self.version))?;
        if !version.pre.is_empty() {
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
    manifest.stable_version()?;
    for asset in &manifest.assets {
        asset.validate()?;
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
}

impl ReleaseState {
    /// Derive durable availability from the running version and release manifest.
    pub fn from_manifest(
        local_version: &str,
        manifest: &ReleaseManifest,
        checked_at_ms: i64,
    ) -> Result<Self> {
        let local = Version::parse(local_version.trim_start_matches('v'))
            .with_context(|| format!("invalid local version `{local_version}`"))?;
        let latest = manifest.stable_version()?;
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
        serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing update state {}", path.display()))
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
    let client = reqwest::Client::builder()
        .user_agent(format!("ainb/{} update-check", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("building GitHub release client")?;
    let manifest_url = format!("{RELEASE_DOWNLOAD_ROOT}/release-manifest.json");
    let signature_url = format!("{RELEASE_DOWNLOAD_ROOT}/release-manifest.sig");
    let manifest_bytes = client
        .get(manifest_url)
        .send()
        .await
        .context("downloading signed release manifest")?
        .error_for_status()
        .context("signed release manifest request failed")?
        .bytes()
        .await
        .context("reading signed release manifest")?;
    let signature = client
        .get(signature_url)
        .send()
        .await
        .context("downloading release manifest signature")?
        .error_for_status()
        .context("release manifest signature request failed")?
        .text()
        .await
        .context("reading release manifest signature")?;
    verify_manifest(&manifest_bytes, &signature)
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
    /// The RUNNING executable decides, and every check is keyed on it. This used
    /// to probe `brew list --versions ainb` first and answer `Homebrew` on
    /// success, without consulting `exe` at all — so on a machine where Homebrew
    /// owns one copy and a second copy shadows it on `PATH`, `ainb update`
    /// upgraded Homebrew's, reported success, and left the binary the user ran
    /// untouched. The shadowing copy could never be updated by the updater, no
    /// matter how many times it was run, and it stayed stale silently.
    ///
    /// That is not hypothetical: a `~/.local/bin/ainb` source build shadowed a
    /// Homebrew 1.24.0 and pinned the whole machine to 1.23.2, which surfaced
    /// only when its embedded migrations were too old to open a database a
    /// newer build had already migrated forward.
    ///
    /// Homebrew is still the answer when the running exe IS Homebrew's, and it
    /// remains the fallback for an exe that matches nothing known — but that
    /// fallback now says out loud which binary it is about to upgrade, because
    /// it is not the one that is running.
    fn detect() -> Result<Self> {
        let exe = std::env::current_exe().context("resolving ainb executable")?;
        let brew_owns = || {
            Command::new("brew")
                .args(["list", "--versions", "ainb"])
                .output()
                .ok()
                .is_some_and(|out| out.status.success())
        };

        if let Some(owner) = classify_exe(
            &exe,
            brew_prefix_binary().as_deref(),
            dirs::home_dir().as_deref(),
        ) {
            return Ok(owner);
        }
        if brew_owns() {
            eprintln!(
                "warning: upgrading the Homebrew ainb, but you are running {}. \
                 That copy shadows Homebrew's on PATH and will stay at this version.",
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
        "https://github.com/stevengonsalvez/agents-in-a-box/releases/download/v{}/{}",
        manifest.stable_version()?,
        asset.archive,
    );
    let bytes = reqwest::Client::builder()
        .user_agent(format!("ainb/{} updater", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(90))
        .build()
        .context("building archive download client")?
        .get(&url)
        .send()
        .await
        .with_context(|| format!("downloading {}", asset.archive))?
        .error_for_status()
        .with_context(|| format!("release archive request failed for {}", asset.archive))?
        .bytes()
        .await
        .context("reading release archive")?;
    let actual_sha = format!("{:x}", Sha256::digest(&bytes));
    if !actual_sha.eq_ignore_ascii_case(asset.sha256.trim()) {
        bail!("release archive checksum mismatch for {}", asset.archive);
    }

    let temp = tempfile::tempdir().context("creating release staging directory")?;
    let archive = temp.path().join(&asset.archive);
    std::fs::write(&archive, &bytes).context("staging verified release archive")?;
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

fn current_target() -> Result<&'static str> {
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
fn classify_exe(
    exe: &Path,
    brew_binary: Option<&Path>,
    home: Option<&Path>,
) -> Option<InstallOwner> {
    if brew_binary.is_some_and(|brewed| canonical_eq(brewed, exe)) {
        return Some(InstallOwner::Homebrew);
    }
    if exe.parent().is_some_and(|parent| parent.ends_with(".cargo/bin")) {
        return Some(InstallOwner::Cargo);
    }
    let direct = [
        PathBuf::from("/usr/local/bin/ainb"),
        home.unwrap_or(Path::new("")).join(".local/bin/ainb"),
    ];
    direct
        .iter()
        .any(|path| canonical_eq(path, exe))
        .then_some(InstallOwner::Direct)
}

/// The `ainb` binary Homebrew owns, if Homebrew has it.
///
/// `brew --prefix ainb` names the keg; the symlink on `PATH` resolves into it,
/// so `canonical_eq` against this is what distinguishes "you are running
/// Homebrew's ainb" from "Homebrew merely has one somewhere".
fn brew_prefix_binary() -> Option<PathBuf> {
    let out = Command::new("brew").args(["--prefix", "ainb"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let prefix = String::from_utf8(out.stdout).ok()?;
    let prefix = prefix.trim();
    (!prefix.is_empty()).then(|| PathBuf::from(prefix).join("bin").join("ainb"))
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
        // Real files: `canonical_eq` requires both sides to resolve, so a fake
        // path matches nothing — which is itself pinned by the last case here.
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let local_bin = home.join(".local/bin");
        let brew_bin = tmp.path().join("brew/bin");
        let cargo_bin = home.join(".cargo/bin");
        for dir in [&local_bin, &brew_bin, &cargo_bin] {
            std::fs::create_dir_all(dir).expect("create dir");
            std::fs::write(dir.join("ainb"), b"binary").expect("write binary");
        }
        let brewed = brew_bin.join("ainb");

        // The regression. Homebrew HAS an ainb, but the running exe is the
        // ~/.local/bin copy that shadows it on PATH. Answering Homebrew here is
        // what let `ainb update` upgrade a binary the user was not running,
        // report success, and leave the shadowing copy stale forever.
        assert_eq!(
            classify_exe(&local_bin.join("ainb"), Some(&brewed), Some(&home)),
            Some(InstallOwner::Direct),
            "a shadowing ~/.local/bin exe owns itself even when Homebrew has a copy"
        );

        assert_eq!(
            classify_exe(&brewed, Some(&brewed), Some(&home)),
            Some(InstallOwner::Homebrew),
            "running Homebrew's own binary is still Homebrew"
        );
        assert_eq!(
            classify_exe(&cargo_bin.join("ainb"), Some(&brewed), Some(&home)),
            Some(InstallOwner::Cargo),
            "a ~/.cargo/bin exe is Cargo's, not Homebrew's"
        );
        assert_eq!(
            classify_exe(
                &tmp.path().join("elsewhere/ainb"),
                Some(&brewed),
                Some(&home)
            ),
            None,
            "an exe matching nothing known must fall through to the probe"
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
