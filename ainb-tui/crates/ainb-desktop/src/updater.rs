//! The desktop updater: a second consumer of the CLI's signed manifest, key
//! and verify function, owning only what differs, the bundle it looks for and
//! the swap of a bundle rather than a file.
//!
//! ```text
//! settings ──channel──▶ root ──Source::manifest──▶ verify (pinned key)
//!    ──version rule (stable | prerelease | off)──▶ desktop bundle for this target
//!    ──Source::download──▶ sha256 of the FILE ──▶ extract ──▶ Info.plist check
//!    ──install owner check ──▶ same filesystem ──▶ swap (previous kept)
//!    ──relaunch ──▶ clear previous once the sidecar connects
//! ```
//!
//! No path skips a step. The host and the key are constants; the test seams
//! (`Updater::with_key`, a fake [`Source`]) exist in debug builds only, and
//! the guard at the bottom refuses a release build that carries them.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ainb_app::cli::update::{
    DesktopBundle, RELEASE_DOWNLOAD_ROOT, ReleaseManifest, ReleaseState, UpdateAvailability,
    current_target, verify_manifest,
};
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The release host every root points into. The prerelease channel forms its
/// root from a tag on this host and nowhere else.
const RELEASE_HOST: &str = "https://github.com/stevengonsalvez/agents-in-a-box";

/// Which manifest the check reads. A local setting of the app, never a field
/// of the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "channel", rename_all = "snake_case")]
pub enum Channel {
    /// `releases/latest/download`, prereleases refused.
    Stable,
    /// One release tag the operator entered, `v<X>-rc<n>`, its own page.
    Prerelease { tag: String },
    /// No request leaves the app.
    Off,
}

impl Channel {
    /// The root to fetch the manifest from, or `None` for `off`. A persisted
    /// `next_root` moves `stable` only; a prerelease tag is pinned to its
    /// release page by construction.
    #[must_use]
    pub fn root(&self, persisted_next_root: Option<&str>) -> Option<String> {
        match self {
            Self::Stable => Some(
                persisted_next_root
                    .map_or_else(|| RELEASE_DOWNLOAD_ROOT.to_string(), str::to_string),
            ),
            Self::Prerelease { tag } => validate_tag(tag)
                .ok()
                .map(|()| format!("{RELEASE_HOST}/releases/download/{tag}")),
            Self::Off => None,
        }
    }

    const fn allows_prerelease(&self) -> bool {
        matches!(self, Self::Prerelease { .. })
    }
}

/// A tag the operator typed: `v` then `x.y.z`, optionally `-` and one
/// alphanumeric segment, the release workflow's own version pattern.
///
/// # Errors
///
/// Anything else, before it can form a URL.
pub fn validate_tag(tag: &str) -> Result<()> {
    let rest = tag.strip_prefix('v').ok_or_else(|| anyhow!("a release tag starts with v"))?;
    let (version, suffix) = match rest.split_once('-') {
        Some((v, s)) => (v, Some(s)),
        None => (rest, None),
    };
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() != 3
        || parts.iter().any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        bail!("a release tag is v<major>.<minor>.<patch>");
    }
    if let Some(suffix) = suffix {
        if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_alphanumeric()) {
            bail!("a prerelease suffix is one alphanumeric segment");
        }
    }
    Ok(())
}

/// The updater's local settings, beside the window's, in the hangar home.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(flatten)]
    pub channel: Channel,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            channel: Channel::Stable,
        }
    }
}

const SETTINGS_FILE: &str = "desktop-updater.json";
const STATE_FILE: &str = "desktop-update-state.json";

impl Settings {
    /// The settings in `home`, or the default when there are none or they
    /// do not parse.
    #[must_use]
    pub fn load(home: &Path) -> Self {
        std::fs::read(home.join(SETTINGS_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Write the settings atomically.
    ///
    /// # Errors
    ///
    /// The home is not writable.
    pub fn save(&self, home: &Path) -> Result<()> {
        write_atomic_json(&home.join(SETTINGS_FILE), self)
    }
}

fn write_atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow!("no parent for {}", path.display()))?;
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("settings")
    ));
    std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
}

/// Where the manifest and the bundles come from. The production source is
/// HTTP; the tests' source is a map.
pub trait Source {
    /// `<root>/release-manifest.json` and `<root>/release-manifest.sig`.
    ///
    /// # Errors
    ///
    /// The request failed.
    fn manifest(&self, root: &str) -> Result<(Vec<u8>, String)>;

    /// Download `url` whole to `to`.
    ///
    /// # Errors
    ///
    /// The request or the write failed.
    fn download(&self, url: &str, to: &Path) -> Result<()>;
}

/// The production source: the CLI's own HTTP client, run on a runtime of its
/// own so it can be called from a blocking thread.
pub struct HttpSource;

impl Source for HttpSource {
    fn manifest(&self, root: &str) -> Result<(Vec<u8>, String)> {
        block_on(ainb_app::cli::update::fetch_manifest_bytes_at(root))
    }

    fn download(&self, url: &str, to: &Path) -> Result<()> {
        block_on(ainb_app::cli::update::download_to(url, to))
    }
}

fn block_on<F: std::future::Future<Output = Result<T>>, T>(future: F) -> Result<T> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building the updater's runtime")?
        .block_on(future)
}

/// How the manifest is verified: always the pinned key in a release build.
enum Verify {
    Pinned,
    #[cfg(debug_assertions)]
    Key(String),
}

impl Verify {
    fn manifest(&self, bytes: &[u8], signature: &str) -> Result<ReleaseManifest> {
        match self {
            Self::Pinned => verify_manifest(bytes, signature),
            #[cfg(debug_assertions)]
            Self::Key(key) => {
                ainb_app::cli::update::verify_manifest_with_key(bytes, signature, key)
            }
        }
    }
}

/// What a check found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Check {
    /// The channel is `off`; nothing was asked.
    Off,
    /// The running build is the newest eligible one.
    Current { running: String, latest: String },
    /// A newer eligible release with a bundle for this target.
    Available {
        version: String,
        bundle: DesktopBundle,
        /// The root the bundle is downloaded from.
        root: String,
    },
    /// A manifest was fetched and refused, or nothing fit; the reason is the
    /// settings section's to show.
    Declined { reason: String },
}

/// The updater over one source, one key and one home.
pub struct Updater {
    source: Arc<dyn Source + Send + Sync>,
    verify: Verify,
    settings: Settings,
    home: PathBuf,
}

impl Updater {
    /// The production updater: HTTP, the pinned key, the home's settings.
    #[must_use]
    pub fn new(home: PathBuf) -> Self {
        Self {
            source: Arc::new(HttpSource),
            verify: Verify::Pinned,
            settings: Settings::load(&home),
            home,
        }
    }

    /// The test seam: a fake source and a throwaway key. Debug builds only.
    #[cfg(debug_assertions)]
    #[must_use]
    pub fn with_key(
        source: Arc<dyn Source + Send + Sync>,
        public_key_b64: &str,
        settings: Settings,
        home: PathBuf,
    ) -> Self {
        Self {
            source,
            verify: Verify::Key(public_key_b64.to_string()),
            settings,
            home,
        }
    }

    /// The channel in force.
    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Replace and persist the settings.
    ///
    /// # Errors
    ///
    /// The home is not writable.
    pub fn set_settings(&mut self, settings: Settings) -> Result<()> {
        settings.save(&self.home)?;
        self.settings = settings;
        Ok(())
    }

    fn state_path(&self) -> PathBuf {
        self.home.join(STATE_FILE)
    }

    /// Checks 1 to 3: fetch, verify with the key, apply the channel's version
    /// rule, find this target's bundle. A verified `next_root` is persisted
    /// so the next check starts there.
    pub fn check(&self, running_version: &str) -> Check {
        let persisted = ReleaseState::load_from(&self.state_path()).ok();
        let Some(root) =
            self.settings.channel.root(persisted.as_ref().and_then(|s| s.root.as_deref()))
        else {
            return Check::Off;
        };
        match self.check_at(&root, running_version) {
            Ok(check) => check,
            Err(error) => Check::Declined {
                reason: format!("{error:#}"),
            },
        }
    }

    fn check_at(&self, root: &str, running_version: &str) -> Result<Check> {
        let (bytes, signature) = self.source.manifest(root)?;
        let manifest = self
            .verify
            .manifest(&bytes, &signature)
            .context("the release manifest's signature did not verify")?;
        let state = ReleaseState::from_manifest_with(
            running_version,
            &manifest,
            now_ms(),
            self.settings.channel.allows_prerelease(),
        )?;
        state.save_to(&self.state_path()).ok();
        if state.availability != UpdateAvailability::Available {
            return Ok(Check::Current {
                running: running_version.to_string(),
                latest: state.latest_version,
            });
        }
        let target = current_target()?;
        let format = if cfg!(target_os = "macos") {
            "dmg"
        } else {
            "appimage"
        };
        let bundle = manifest.desktop_bundle_for(target, format).cloned().ok_or_else(|| {
            anyhow!(
                "release {} has no desktop bundle for {target} as {format}",
                manifest.version
            )
        })?;
        Ok(Check::Available {
            version: state.latest_version,
            bundle,
            root: root.to_string(),
        })
    }

    /// Check 4: download the bundle's archive into `staging` and hash the
    /// file. On any failure the staging directory is removed.
    ///
    /// # Errors
    ///
    /// The download failed or the checksum did not match.
    pub fn download_and_verify(
        &self,
        bundle: &DesktopBundle,
        root: &str,
        staging: &Path,
    ) -> Result<PathBuf> {
        let result = (|| {
            std::fs::create_dir_all(staging)?;
            let file = staging.join(&bundle.archive);
            let url = format!("{}/{}", root.trim_end_matches('/'), bundle.archive);
            self.source.download(&url, &file)?;
            let bytes = std::fs::read(&file)?;
            let actual = format!("{:x}", Sha256::digest(&bytes));
            if !actual.eq_ignore_ascii_case(bundle.sha256.trim()) {
                bail!("checksum mismatch for {}", bundle.archive);
            }
            Ok(file)
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir_all(staging);
        }
        result
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// What the running executable is installed as, and therefore what a swap
/// may replace. Check 6.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Install {
    /// A macOS bundle the user can write, with its previous beside it.
    Bundle { app: PathBuf, previous: PathBuf },
    /// A Linux AppImage file the user can write.
    AppImage { file: PathBuf },
}

impl Install {
    /// The install from the running executable's path.
    ///
    /// # Errors
    ///
    /// A path the updater must not replace, named with the fix.
    pub fn detect() -> Result<Self> {
        if let Some(appimage) = std::env::var_os("APPIMAGE") {
            return Self::detect_from_appimage(Path::new(&appimage));
        }
        let exe = std::env::current_exe().context("resolving the desktop executable")?;
        Self::detect_from(&exe)
    }

    /// [`Self::detect`] for an explicit executable path inside a bundle.
    ///
    /// # Errors
    ///
    /// As [`Self::detect`].
    pub fn detect_from(exe: &Path) -> Result<Self> {
        let resolved = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
        if resolved
            .ancestors()
            .any(|p| p.file_name().is_some_and(|n| n == "Cellar" || n == "Caskroom"))
        {
            bail!("this install belongs to Homebrew; update it with brew");
        }
        let Some(app) = resolved.ancestors().find(|p| p.extension().is_some_and(|e| e == "app"))
        else {
            if resolved.components().any(|c| c.as_os_str() == "usr") {
                bail!("this install belongs to a package manager; update it there");
            }
            bail!("the running executable is not inside an app bundle");
        };
        if resolved.components().any(|c| c.as_os_str() == "Volumes") {
            bail!("the app is running from a disk image; copy it to Applications first");
        }
        let parent = app.parent().ok_or_else(|| anyhow!("the bundle has no parent"))?;
        let probe = parent.join(".ainb-desktop-write-probe");
        std::fs::write(&probe, b"")
            .with_context(|| format!("{} is not writable by this user", parent.display()))?;
        let _ = std::fs::remove_file(&probe);
        let name = app
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow!("the bundle has no name"))?;
        Ok(Self::Bundle {
            app: app.to_path_buf(),
            previous: parent.join(format!("{name}.previous")),
        })
    }

    /// The AppImage the runtime says it mounted (`$APPIMAGE`).
    ///
    /// # Errors
    ///
    /// The file cannot be replaced.
    pub fn detect_from_appimage(file: &Path) -> Result<Self> {
        if file.components().any(|c| c.as_os_str() == "usr") {
            bail!("this install belongs to a package manager; update it there");
        }
        let parent = file.parent().ok_or_else(|| anyhow!("the AppImage has no parent"))?;
        let probe = parent.join(".ainb-desktop-write-probe");
        std::fs::write(&probe, b"")
            .with_context(|| format!("{} is not writable by this user", parent.display()))?;
        let _ = std::fs::remove_file(&probe);
        Ok(Self::AppImage {
            file: file.to_path_buf(),
        })
    }

    /// Where the previous copy goes.
    #[must_use]
    pub fn previous(&self) -> PathBuf {
        match self {
            Self::Bundle { previous, .. } => previous.clone(),
            Self::AppImage { file } => file.with_extension("AppImage.previous"),
        }
    }

    /// What is replaced.
    #[must_use]
    pub fn current(&self) -> &Path {
        match self {
            Self::Bundle { app, .. } => app,
            Self::AppImage { file } => file,
        }
    }
}

/// Check 7 and the swap: `staged` moves into place with the current kept as
/// the previous. A rename, never a copy, so both must sit on one filesystem;
/// a failed second rename restores the first.
///
/// # Errors
///
/// A rename failed; the install is as it was.
pub fn swap(install: &Install, staged: &Path) -> Result<()> {
    let current = install.current();
    let previous = install.previous();
    strip_quarantine(staged)?;
    if previous.exists() {
        remove_path(&previous)?;
    }
    if current.exists() {
        std::fs::rename(current, &previous)
            .with_context(|| format!("moving {} aside", current.display()))?;
    }
    if let Err(error) = std::fs::rename(staged, current) {
        if previous.exists() {
            let _ = std::fs::rename(&previous, current);
        }
        return Err(error)
            .with_context(|| format!("moving the new bundle into {}", current.display()));
    }
    Ok(())
}

/// Put the previous back, while it exists.
///
/// # Errors
///
/// There is no previous, or a rename failed.
pub fn rollback(install: &Install) -> Result<()> {
    let current = install.current();
    let previous = install.previous();
    if !previous.exists() {
        bail!("there is no previous version to roll back to");
    }
    let parked = current.with_extension("rolled-back");
    if parked.exists() {
        remove_path(&parked)?;
    }
    if current.exists() {
        std::fs::rename(current, &parked)?;
    }
    if let Err(error) = std::fs::rename(&previous, current) {
        let _ = std::fs::rename(&parked, current);
        return Err(error).context("restoring the previous version");
    }
    let _ = remove_path(&parked);
    Ok(())
}

/// Remove the previous copy, once the new one has proved itself.
///
/// # Errors
///
/// The removal failed.
pub fn clear_previous(install: &Install) -> Result<()> {
    let previous = install.previous();
    if previous.exists() {
        remove_path(&previous)?;
    }
    Ok(())
}

/// Finish a swap a crash interrupted: a `<name>.next` beside a missing
/// `<name>` moves in. `Ok(true)` when something moved.
///
/// # Errors
///
/// The rename failed.
pub fn repair_interrupted_swap(current: &Path) -> Result<bool> {
    if current.exists() {
        return Ok(false);
    }
    let name = current
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("no bundle name"))?;
    let next = current.with_file_name(format!("{name}.next"));
    if !next.exists() {
        return Ok(false);
    }
    std::fs::rename(&next, current).context("finishing an interrupted update")?;
    Ok(true)
}

/// The staging name a swap uses beside the install, so a crash leaves a
/// `<name>.next` the next start can finish.
#[must_use]
pub fn next_path(install: &Install) -> PathBuf {
    let current = install.current();
    let name = current.file_name().and_then(|n| n.to_str()).unwrap_or("bundle");
    current.with_file_name(format!("{name}.next"))
}

fn remove_path(path: &Path) -> Result<()> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
    .with_context(|| format!("removing {}", path.display()))
}

/// Drop `com.apple.quarantine` from a staged bundle, so an ad-hoc signed
/// bundle relaunches without a Gatekeeper prompt. The updater writes its
/// downloads with plain file writes, which set none; this covers a tool that
/// did. No-op off macOS.
fn strip_quarantine(path: &Path) -> Result<()> {
    if !cfg!(target_os = "macos") {
        return Ok(());
    }
    let status = std::process::Command::new("xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(path)
        .output()
        .context("running xattr")?;
    // xattr exits non-zero when the attribute is absent; the state after is
    // what matters, checked below.
    let _ = status;
    let listed = std::process::Command::new("xattr")
        .arg("-l")
        .arg(path)
        .output()
        .context("listing attributes")?;
    if String::from_utf8_lossy(&listed.stdout).contains("com.apple.quarantine") {
        bail!("the staged bundle still carries com.apple.quarantine");
    }
    Ok(())
}

// A release build carries no test seam of the updater: `Updater::with_key`
// and the `Verify::Key` arm exist only under debug assertions, and this
// refuses a release build that somehow has them.
#[cfg(all(test, not(debug_assertions)))]
compile_error!("the updater's test seams must never be built into a release binary");
