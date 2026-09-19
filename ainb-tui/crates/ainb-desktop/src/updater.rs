//! The desktop updater: a second consumer of the CLI's signed manifest, key
//! and verify function, owning only what differs, the bundle it looks for and
//! the swap of a bundle rather than a file.
//!
//! ```text
//! settings ──channel──▶ root ──Source::manifest──▶ verify (pinned key)
//!    ──version rule (stable | prerelease | off)──▶ desktop bundle for this target
//!    ──Source::download──▶ sha256 of the FILE ──▶ extract ──▶ Info.plist check
//!    ──install owner check ──▶ same filesystem ──▶ swap (previous kept)
//!    ──relaunch; the previous stays as the one rollback slot until the next
//!      applied update replaces it or the person removes it
//! ```
//!
//! No path skips a step. The host and the key are constants; the test seams
//! (`Updater::with_key`, a fake [`Source`]) exist under the `test-seams`
//! feature only, which this crate's own dev-dependency turns on for its tests
//! and nothing else does; the guard at the bottom refuses a release build
//! that carries them.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ainb_app::cli::update::{
    DesktopBundle, RELEASE_DOWNLOAD_ROOT, ReleaseManifest, ReleaseState, UpdateAvailability,
    current_target, release_host, verify_manifest,
};
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
    /// release page on the CLI's one release host by construction.
    ///
    /// # Errors
    ///
    /// A prerelease tag that does not parse, so the check declines with the
    /// reason rather than reading as `off`.
    pub fn root(&self, persisted_next_root: Option<&str>) -> Result<Option<String>> {
        Ok(match self {
            Self::Stable => Some(
                persisted_next_root
                    .map_or_else(|| RELEASE_DOWNLOAD_ROOT.to_string(), str::to_string),
            ),
            Self::Prerelease { tag } => {
                validate_tag(tag).with_context(|| format!("the prerelease tag `{tag}`"))?;
                Some(format!("{}/releases/download/{tag}", release_host()))
            }
            Self::Off => None,
        })
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
    /// The settings in `home`, or the default when there is no file.
    ///
    /// # Errors
    ///
    /// A file that exists but cannot be read or does not parse. That is not
    /// the default: a file set to `off` and then damaged must not start
    /// fetching from `stable`, so the updater declines until it is fixed.
    pub fn load(home: &Path) -> Result<Self> {
        let path = home.join(SETTINGS_FILE);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", path.display()));
            }
        };
        serde_json::from_slice(&bytes)
            .with_context(|| format!("{SETTINGS_FILE} in the hangar home does not parse"))
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

    /// Download `url` to `to`, reporting (bytes so far, declared total) as
    /// it lands.
    ///
    /// # Errors
    ///
    /// The request or the write failed.
    fn download(
        &self,
        url: &str,
        to: &Path,
        progress: &mut (dyn FnMut(u64, Option<u64>) + Send),
    ) -> Result<()>;
}

/// The production source: the CLI's own HTTP client (https pinned, redirects
/// kept on host, bodies capped, the bundle streamed), run on a runtime of its
/// own so it can be called from a blocking thread.
pub struct HttpSource;

impl Source for HttpSource {
    fn manifest(&self, root: &str) -> Result<(Vec<u8>, String)> {
        block_on(ainb_app::cli::update::fetch_manifest_bytes_at(root))
    }

    fn download(
        &self,
        url: &str,
        to: &Path,
        progress: &mut (dyn FnMut(u64, Option<u64>) + Send),
    ) -> Result<()> {
        block_on(ainb_app::cli::update::download_to(url, to, progress)).map(|_hash| ())
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
    #[cfg(feature = "test-seams")]
    Key(String),
}

impl Verify {
    fn manifest(&self, bytes: &[u8], signature: &str) -> Result<ReleaseManifest> {
        match self {
            Self::Pinned => verify_manifest(bytes, signature),
            #[cfg(feature = "test-seams")]
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

/// Where an apply stands, for the window to frame. The download reports as
/// chunks land; the rest are one event each.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum Phase {
    Checking,
    Downloading { received: u64, total: Option<u64> },
    Verifying,
    Applying,
    Installed { version: String },
    Failed { reason: String },
}

/// The updater over one source, one key and one home.
pub struct Updater {
    source: Arc<dyn Source + Send + Sync>,
    verify: Verify,
    settings: Settings,
    /// Why the settings file could not be read, when it could not: every
    /// check declines with it until the file is fixed.
    settings_fault: Option<String>,
    home: PathBuf,
    /// Set once an apply swapped a bundle in: the next apply is refused until
    /// the restart, so one install is in flight at a time and the rollback
    /// slot keeps the version that was running before it.
    applied: AtomicBool,
}

impl Updater {
    /// The production updater: HTTP, the pinned key, the home's settings.
    #[must_use]
    pub fn new(home: PathBuf) -> Self {
        // A file that does not read is `off` with the reason, never the
        // default: nothing is fetched or applied until it is fixed.
        let (settings, settings_fault) = match Settings::load(&home) {
            Ok(settings) => (settings, None),
            Err(error) => (
                Settings {
                    channel: Channel::Off,
                },
                Some(format!("{error:#}")),
            ),
        };
        Self {
            source: Arc::new(HttpSource),
            verify: Verify::Pinned,
            settings,
            settings_fault,
            home,
            applied: AtomicBool::new(false),
        }
    }

    /// The test seam: a fake source and a throwaway key. `test-seams` only.
    #[cfg(feature = "test-seams")]
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
            settings_fault: None,
            home,
            applied: AtomicBool::new(false),
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
        self.settings_fault = None;
        Ok(())
    }

    fn state_path(&self) -> PathBuf {
        self.home.join(STATE_FILE)
    }

    /// Checks 1 to 3: fetch, verify with the key, apply the channel's version
    /// rule, find this target's bundle. A verified `next_root` is persisted
    /// so the next check starts there, and stays until another replaces it.
    pub fn check(&self, running_version: &str) -> Check {
        if let Some(fault) = &self.settings_fault {
            return Check::Declined {
                reason: fault.clone(),
            };
        }
        let persisted =
            ReleaseState::load_from(&self.state_path()).ok().and_then(|state| state.root);
        let root = match self.settings.channel.root(persisted.as_deref()) {
            Ok(Some(root)) => root,
            Ok(None) => return Check::Off,
            Err(error) => {
                return Check::Declined {
                    reason: format!("{error:#}"),
                };
            }
        };
        match self.check_at(&root, running_version, persisted) {
            Ok(check) => check,
            Err(error) => Check::Declined {
                reason: format!("{error:#}"),
            },
        }
    }

    fn check_at(
        &self,
        root: &str,
        running_version: &str,
        persisted_root: Option<String>,
    ) -> Result<Check> {
        let (bytes, signature) = self.source.manifest(root)?;
        let manifest = self
            .verify
            .manifest(&bytes, &signature)
            .context("the release manifest's signature did not verify")?;
        let mut state = ReleaseState::from_manifest_with(
            running_version,
            &manifest,
            now_ms(),
            self.settings.channel.allows_prerelease(),
        )?;
        // A move sticks: a manifest that names no next_root keeps the root
        // the last one moved the check to.
        if state.root.is_none() {
            state.root = persisted_root;
        }
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

    /// Check 4: download the bundle's archive into `staging` (a directory of
    /// this user's alone) and hash the file. On any failure the staging
    /// directory is removed.
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
        self.download_and_verify_with(bundle, root, staging, &mut |_| {})
    }

    /// [`Self::download_and_verify`] reporting its phases.
    ///
    /// # Errors
    ///
    /// As [`Self::download_and_verify`].
    pub fn download_and_verify_with(
        &self,
        bundle: &DesktopBundle,
        root: &str,
        staging: &Path,
        phase: &mut (dyn FnMut(Phase) + Send),
    ) -> Result<PathBuf> {
        let result = (|| {
            create_private_dir(staging)?;
            let file = staging.join(&bundle.archive);
            let url = format!("{}/{}", root.trim_end_matches('/'), bundle.archive);
            self.source.download(&url, &file, &mut |received, total| {
                phase(Phase::Downloading { received, total });
            })?;
            phase(Phase::Verifying);
            verify_file_hash(&file, &bundle.sha256)?;
            Ok(file)
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir_all(staging);
        }
        result
    }
}

/// `dir`, created for this user alone (0700), so nothing else on the machine
/// can swap the file between the hash and the mount.
fn create_private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// The sha256 of the FILE at `path` against `expected`, streamed.
fn verify_file_hash(path: &Path, expected: &str) -> Result<()> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    let actual = format!("{:x}", hasher.finalize());
    if !actual.eq_ignore_ascii_case(expected.trim()) {
        bail!(
            "checksum mismatch for {}",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("the download")
        );
    }
    Ok(())
}

impl Updater {
    /// Checks 4 to 7 and the swap, from an [`Check::Available`]: download
    /// beside the install (one filesystem, so the swap is a rename), hash the
    /// file, unpack the bundle, read its version back as data, drop any
    /// quarantine attribute, and move it into place with the previous kept.
    /// Returns the path now holding the new version.
    ///
    /// # Errors
    ///
    /// Any check that failed, with the staging removed; the channel is `off`;
    /// an update is already installed and waiting for the restart.
    pub fn apply(&self, check: &Check, install: &Install) -> Result<PathBuf> {
        self.apply_with(check, install, &mut |_| {})
    }

    /// [`Self::apply`] reporting its phases: downloading (as chunks land),
    /// verifying, applying.
    ///
    /// # Errors
    ///
    /// As [`Self::apply`].
    pub fn apply_with(
        &self,
        check: &Check,
        install: &Install,
        phase: &mut (dyn FnMut(Phase) + Send),
    ) -> Result<PathBuf> {
        let Check::Available {
            version,
            bundle,
            root,
        } = check
        else {
            bail!("nothing to install: the last check found no update");
        };
        if self.settings.channel == Channel::Off {
            bail!("updates are off in this app's settings");
        }
        if self.applied.load(Ordering::SeqCst) {
            bail!("an update is already installed; restart to use it");
        }
        let next = next_path(install);
        let staging = next.with_extension("staging");
        let _ = std::fs::remove_dir_all(&staging);
        let _ = remove_path(&next);
        let result = (|| {
            let archive = self.download_and_verify_with(bundle, root, &staging, phase)?;
            phase(Phase::Applying);
            // The file was hashed once it landed; it is hashed again right
            // before it is opened, so the mount sees what was verified.
            verify_file_hash(&archive, &bundle.sha256)?;
            match bundle.format.as_str() {
                "dmg" => extract_dmg(&archive, &next)?,
                "appimage" => {
                    std::fs::rename(&archive, &next)?;
                    make_executable(&next)?;
                }
                other => bail!("no installer for a {other} bundle"),
            }
            if cfg!(target_os = "macos") {
                let found = bundle_version(&next)?;
                if found != *version {
                    bail!("the downloaded bundle says it is {found}, the manifest said {version}");
                }
            }
            Ok(())
        })();
        let _ = std::fs::remove_dir_all(&staging);
        if let Err(error) = result {
            let _ = remove_path(&next);
            return Err(error);
        }
        swap(install, &next)?;
        self.applied.store(true, Ordering::SeqCst);
        Ok(install.current().to_path_buf())
    }
}

/// Check 5, as data: `CFBundleShortVersionString` from the bundle's own
/// `Info.plist`, read with `plutil`, never by running the download.
fn bundle_version(app: &Path) -> Result<String> {
    let output = std::process::Command::new("plutil")
        .args(["-extract", "CFBundleShortVersionString", "raw"])
        .arg(app.join("Contents/Info.plist"))
        .output()
        .context("running plutil")?;
    if !output.status.success() {
        bail!("the downloaded bundle has no readable Info.plist");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Mount `dmg` read-only, copy the one `.app` inside it to `dest`, unmount.
fn extract_dmg(dmg: &Path, dest: &Path) -> Result<()> {
    let mount = tempfile::tempdir().context("creating a mount point")?;
    let attached = std::process::Command::new("hdiutil")
        .args([
            "attach",
            "-nobrowse",
            "-readonly",
            "-noautoopen",
            "-mountpoint",
        ])
        .arg(mount.path())
        .arg(dmg)
        .output()
        .context("running hdiutil attach")?;
    if !attached.status.success() {
        bail!(
            "the disk image did not mount: {}",
            String::from_utf8_lossy(&attached.stderr).trim()
        );
    }
    let copied = (|| {
        let app = std::fs::read_dir(mount.path())?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.extension().is_some_and(|e| e == "app"))
            .ok_or_else(|| anyhow!("the disk image carries no .app"))?;
        let status = std::process::Command::new("ditto")
            .arg(&app)
            .arg(dest)
            .status()
            .context("running ditto")?;
        if !status.success() {
            bail!("copying the app out of the disk image failed");
        }
        Ok(())
    })();
    let _ = std::process::Command::new("hdiutil")
        .args(["detach", "-quiet"])
        .arg(mount.path())
        .status();
    copied
}

fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
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

/// Remove the previous copy, the one rollback slot. Only the person does
/// this, or the next applied update by replacing it; nothing clears it on
/// its own.
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

/// The startup half of the repair: when this process runs from a
/// `<name>.previous` or `<name>.next` (the only things left to launch after
/// a crash between the swap's two renames), finish the move-in of
/// `<name>.next` so the next launch is the new version. `Ok(true)` when
/// something moved.
///
/// # Errors
///
/// The rename failed.
pub fn repair_at_startup() -> Result<bool> {
    let exe = std::env::current_exe().context("resolving the desktop executable")?;
    let appimage = std::env::var_os("APPIMAGE").map(PathBuf::from);
    repair_at_startup_from(&exe, appimage.as_deref())
}

/// [`repair_at_startup`] from what the process knows about itself: on Linux
/// the AppImage runtime mounts the file and names it in `$APPIMAGE`, so the
/// executable's own path is inside a mount and says nothing; on macOS the
/// executable sits inside the bundle.
///
/// # Errors
///
/// The rename failed.
pub fn repair_at_startup_from(exe: &Path, appimage: Option<&Path>) -> Result<bool> {
    let parked = if let Some(appimage) = appimage {
        appimage
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| n.ends_with(".AppImage.previous") || n.ends_with(".AppImage.next"))
            .map(|_| appimage.to_path_buf())
    } else {
        exe.ancestors()
            .find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(".app.previous") || n.ends_with(".app.next"))
            })
            .map(Path::to_path_buf)
    };
    let Some(parked) = parked else {
        return Ok(false);
    };
    let name = parked
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".previous").or_else(|| n.strip_suffix(".next")))
        .ok_or_else(|| anyhow!("no bundle name"))?;
    repair_interrupted_swap(&parked.with_file_name(name))
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
    // Recursive: Gatekeeper reads the inner files too.
    let listed = std::process::Command::new("xattr")
        .arg("-lr")
        .arg(path)
        .output()
        .context("listing attributes")?;
    if String::from_utf8_lossy(&listed.stdout).contains("com.apple.quarantine") {
        bail!("the staged bundle still carries com.apple.quarantine");
    }
    Ok(())
}

// A release build carries no test seam of the updater: `Updater::with_key`
// and the `Verify::Key` arm exist only under the `test-seams` feature, which
// only this crate's own dev-dependency enables, and this refuses a release
// build that somehow has it.
#[cfg(all(feature = "test-seams", not(debug_assertions)))]
compile_error!("the updater's test seams must never be built into a release binary");
