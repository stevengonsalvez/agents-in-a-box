//! The desktop updater against a fake source and fixture bundles: every
//! refusal in the goal's list, the swap, the kept previous, the rollback,
//! `off` making no request, and the startup repair of an interrupted swap.
//!
//! The manifest is signed with a throwaway key through `Updater::with_key`,
//! which exists under `cfg(test)`-shaped gates only (see the compile guard in
//! `updater.rs`); the key seam is never in a release build.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;

use ainb_desktop::updater::{
    Channel, Check, Install, Phase, Settings, Source, Updater, clear_previous,
    repair_at_startup_from, repair_interrupted_swap, rollback, swap, validate_tag,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

/// A source that serves what the test put in it and counts every request.
#[derive(Default)]
struct FakeSource {
    manifests: HashMap<String, (Vec<u8>, String)>,
    files: HashMap<String, Vec<u8>>,
    requests: Mutex<Vec<String>>,
}

impl Source for FakeSource {
    fn manifest(&self, root: &str) -> anyhow::Result<(Vec<u8>, String)> {
        self.requests.lock().unwrap().push(format!("manifest {root}"));
        self.manifests
            .get(root)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("404 for {root}"))
    }

    fn download(
        &self,
        url: &str,
        to: &Path,
        progress: &mut (dyn FnMut(u64, Option<u64>) + Send),
    ) -> anyhow::Result<()> {
        self.requests.lock().unwrap().push(format!("download {url}"));
        let bytes = self.files.get(url).ok_or_else(|| anyhow::anyhow!("404 for {url}"))?;
        std::fs::write(to, bytes)?;
        progress(bytes.len() as u64, Some(bytes.len() as u64));
        Ok(())
    }
}

fn key() -> SigningKey {
    SigningKey::from_bytes(&[21; 32])
}

fn signed(key: &SigningKey, body: &str) -> (Vec<u8>, String) {
    let sig = STANDARD.encode(key.sign(body.as_bytes()).to_bytes());
    (body.as_bytes().to_vec(), sig)
}

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The stable channel's root, which the fake source answers for.
const ROOT: &str = ainb_app::cli::update::RELEASE_DOWNLOAD_ROOT;

/// A manifest for `version` whose one desktop bundle is `archive` with `sha256`.
fn manifest(version: &str, archive: &str, sha256: &str, extra: &str) -> String {
    let target = ainb_app::cli::update::current_target().unwrap();
    let format = if cfg!(target_os = "macos") {
        "dmg"
    } else {
        "appimage"
    };
    format!(
        r#"{{"version":"{version}","assets":[],"desktop":[{{"target":"{target}","format":"{format}","archive":"{archive}","sha256":"{sha256}","signed":false}}]{extra}}}"#
    )
}

fn updater(source: Arc<FakeSource>, channel: Channel, home: &Path) -> Updater {
    Updater::with_key(
        source,
        &STANDARD.encode(key().verifying_key().as_bytes()),
        Settings { channel },
        home.to_path_buf(),
    )
}

// ── the channel ─────────────────────────────────────────────────────────────

#[test]
fn off_makes_no_request_at_all() {
    let home = tempfile::tempdir().unwrap();
    let source = Arc::new(FakeSource::default());
    let u = updater(Arc::clone(&source), Channel::Off, home.path());
    assert!(matches!(u.check("1.28.2"), Check::Off));
    assert!(
        source.requests.lock().unwrap().is_empty(),
        "{:?}",
        source.requests.lock().unwrap()
    );
}

#[test]
fn a_prerelease_tag_is_validated_before_it_forms_a_url() {
    assert!(validate_tag("v1.29.0-rc1").is_ok());
    assert!(validate_tag("v1.29.0").is_ok());
    for bad in [
        "1.29.0-rc1",
        "v1.29",
        "v1.29.0-rc.1",
        "v1.29.0/../x",
        "",
        "v1.29.0 rc1",
    ] {
        assert!(validate_tag(bad).is_err(), "{bad} was accepted");
    }
    let stable = Channel::Stable;
    assert_eq!(
        stable.root(None).unwrap().as_deref(),
        Some(ainb_app::cli::update::RELEASE_DOWNLOAD_ROOT)
    );
    let pre = Channel::Prerelease {
        tag: "v1.29.0-rc1".into(),
    };
    // The prerelease root is formed on the CLI's release host, the one
    // source, and nowhere else.
    assert_eq!(
        pre.root(None).unwrap().as_deref(),
        Some(
            format!(
                "{}/releases/download/v1.29.0-rc1",
                ainb_app::cli::update::release_host()
            )
            .as_str()
        )
    );
    // A persisted next_root wins for stable; the prerelease tag stays pinned
    // to its release page.
    assert_eq!(
        stable.root(Some("https://example.org/new")).unwrap().as_deref(),
        Some("https://example.org/new")
    );
    assert!(Channel::Off.root(None).unwrap().is_none());
    // A bad tag is an error, not a silent `off`.
    assert!(
        Channel::Prerelease {
            tag: "v1.29".into()
        }
        .root(None)
        .is_err()
    );
}

/// A prerelease tag that does not parse declines the check with the reason,
/// rather than reading as `off`; nothing is requested.
#[test]
fn an_invalid_tag_declines_the_check_with_its_reason() {
    let home = tempfile::tempdir().unwrap();
    let source = Arc::new(FakeSource::default());
    let u = updater(
        Arc::clone(&source),
        Channel::Prerelease {
            tag: "v1.29.0/../x".into(),
        },
        home.path(),
    );
    match u.check("1.28.2") {
        Check::Declined { reason } => assert!(reason.contains("tag"), "{reason}"),
        other => panic!("{other:?}"),
    }
    assert!(source.requests.lock().unwrap().is_empty());
}

#[test]
fn settings_round_trip_through_the_home_and_default_to_stable() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(
        Settings::load(home.path()).unwrap().channel,
        Channel::Stable
    );
    let s = Settings {
        channel: Channel::Prerelease {
            tag: "v1.29.0-rc2".into(),
        },
    };
    s.save(home.path()).unwrap();
    assert_eq!(Settings::load(home.path()).unwrap(), s);
}

/// A settings file that does not parse is not the default: a file that was
/// set to `off` and then damaged must not start fetching from `stable`.
/// Every check declines with the file named until it is fixed.
#[test]
fn an_unparsable_settings_file_fails_closed_with_its_reason() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("desktop-updater.json"),
        br#"{"channel": "nonsense""#,
    )
    .unwrap();
    assert!(Settings::load(home.path()).is_err());
    let u = Updater::new(home.path().to_path_buf());
    match u.check("1.28.2") {
        Check::Declined { reason } => {
            assert!(reason.contains("desktop-updater.json"), "{reason}");
        }
        other => panic!("{other:?}"),
    }
}

/// An update is installed on the channel that found it: `off` set after the
/// check refuses the apply.
#[test]
fn apply_refuses_when_the_channel_is_off() {
    let home = tempfile::tempdir().unwrap();
    let mut source = FakeSource::default();
    let archive = "ainb-desktop-1.29.0-x.AppImage";
    let bytes = b"the bundle bytes".to_vec();
    source.manifests.insert(
        ROOT.into(),
        signed(&key(), &manifest("1.29.0", archive, &sha(&bytes), "")),
    );
    source.files.insert(format!("{ROOT}/{archive}"), bytes);
    let source = Arc::new(source);
    let mut u = updater(Arc::clone(&source), Channel::Stable, home.path());
    let check = u.check("1.28.2");
    assert!(matches!(check, Check::Available { .. }), "{check:?}");
    u.set_settings(Settings {
        channel: Channel::Off,
    })
    .unwrap();
    let install = Install::AppImage {
        file: home.path().join("ainb.AppImage"),
    };
    let error = u.apply(&check, &install).unwrap_err();
    assert!(error.to_string().contains("off"), "{error:#}");
    assert_eq!(
        source.requests.lock().unwrap().len(),
        1,
        "the apply made a request"
    );
}

// ── the checks, in order ───────────────────────────────────────────────────

#[test]
fn a_manifest_signed_by_another_key_is_declined() {
    let home = tempfile::tempdir().unwrap();
    let mut source = FakeSource::default();
    let other = SigningKey::from_bytes(&[22; 32]);
    source.manifests.insert(
        ROOT.into(),
        signed(
            &other,
            &manifest("1.29.0", "ainb-desktop-1.29.0-x.dmg", &"0".repeat(64), ""),
        ),
    );
    let u = updater(Arc::new(source), Channel::Stable, home.path());
    match u.check("1.28.2") {
        Check::Declined { reason } => assert!(reason.contains("signature"), "{reason}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_prerelease_is_declined_on_stable_and_accepted_on_prerelease() {
    let home = tempfile::tempdir().unwrap();
    let mut source = FakeSource::default();
    let body = manifest(
        "1.29.0-rc2",
        "ainb-desktop-1.29.0-rc2-x.dmg",
        &"0".repeat(64),
        "",
    );
    source.manifests.insert(ROOT.into(), signed(&key(), &body));
    let pre_root =
        "https://github.com/stevengonsalvez/agents-in-a-box/releases/download/v1.29.0-rc2";
    source.manifests.insert(pre_root.into(), signed(&key(), &body));
    let source = Arc::new(source);
    let u = updater(Arc::clone(&source), Channel::Stable, home.path());
    assert!(matches!(u.check("1.29.0-rc1"), Check::Declined { .. }));
    let u = updater(
        Arc::clone(&source),
        Channel::Prerelease {
            tag: "v1.29.0-rc2".into(),
        },
        home.path(),
    );
    match u.check("1.29.0-rc1") {
        Check::Available { version, .. } => assert_eq!(version, "1.29.0-rc2"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_equal_or_older_release_reports_current() {
    let home = tempfile::tempdir().unwrap();
    let mut source = FakeSource::default();
    source.manifests.insert(
        ROOT.into(),
        signed(
            &key(),
            &manifest("1.28.2", "ainb-desktop-1.28.2-x.dmg", &"0".repeat(64), ""),
        ),
    );
    let u = updater(Arc::new(source), Channel::Stable, home.path());
    assert!(matches!(u.check("1.28.2"), Check::Current { .. }));
    assert!(matches!(u.check("1.30.0"), Check::Current { .. }));
}

#[test]
fn a_release_with_no_bundle_for_this_target_is_declined() {
    let home = tempfile::tempdir().unwrap();
    let mut source = FakeSource::default();
    source.manifests.insert(
        ROOT.into(),
        signed(&key(), r#"{"version":"1.29.0","assets":[],"desktop":[]}"#),
    );
    let u = updater(Arc::new(source), Channel::Stable, home.path());
    match u.check("1.28.2") {
        Check::Declined { reason } => assert!(reason.contains("no desktop bundle"), "{reason}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_checksum_mismatch_is_declined_and_leaves_no_staging_behind() {
    let home = tempfile::tempdir().unwrap();
    let mut source = FakeSource::default();
    let archive = "ainb-desktop-1.29.0-x.AppImage";
    source.manifests.insert(
        ROOT.into(),
        signed(&key(), &manifest("1.29.0", archive, &"0".repeat(64), "")),
    );
    source.files.insert(
        format!("{ROOT}/{archive}"),
        b"not the bytes the manifest hashed".to_vec(),
    );
    let source = Arc::new(source);
    let u = updater(Arc::clone(&source), Channel::Stable, home.path());
    let Check::Available { bundle, root, .. } = u.check("1.28.2") else {
        panic!("expected available");
    };
    let staging = home.path().join("staging");
    let err = u.download_and_verify(&bundle, &root, &staging).unwrap_err();
    assert!(err.to_string().contains("checksum"), "{err}");
    assert!(!staging.exists(), "staging was left behind");
}

#[test]
fn a_verified_archive_lands_in_staging_and_the_file_is_what_was_hashed() {
    let home = tempfile::tempdir().unwrap();
    let mut source = FakeSource::default();
    let archive = "ainb-desktop-1.29.0-x.AppImage";
    let bytes = b"the bundle bytes".to_vec();
    source.manifests.insert(
        ROOT.into(),
        signed(&key(), &manifest("1.29.0", archive, &sha(&bytes), "")),
    );
    source.files.insert(format!("{ROOT}/{archive}"), bytes.clone());
    let source = Arc::new(source);
    let u = updater(Arc::clone(&source), Channel::Stable, home.path());
    let Check::Available { bundle, root, .. } = u.check("1.28.2") else {
        panic!("expected available");
    };
    let staging = home.path().join("staging");
    let file = u.download_and_verify(&bundle, &root, &staging).unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&staging).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "the staging directory is this user's alone");
    }
    assert_eq!(
        source.requests.lock().unwrap().last().unwrap(),
        &format!("download {ROOT}/{archive}")
    );
}

#[test]
fn a_verified_next_root_is_persisted_and_used_by_the_next_check() {
    let home = tempfile::tempdir().unwrap();
    let mut source = FakeSource::default();
    let new_root = "https://example.org/new-home/releases/latest/download";
    source.manifests.insert(
        ROOT.into(),
        signed(
            &key(),
            &manifest(
                "1.28.2",
                "a.dmg",
                &"0".repeat(64),
                &format!(r#","next_root":"{new_root}""#),
            ),
        ),
    );
    source.manifests.insert(
        new_root.into(),
        signed(
            &key(),
            &manifest("1.30.0", "ainb-desktop-1.30.0-x.dmg", &"0".repeat(64), ""),
        ),
    );
    let source = Arc::new(source);
    let u = updater(Arc::clone(&source), Channel::Stable, home.path());
    assert!(matches!(u.check("1.28.2"), Check::Current { .. }));
    match u.check("1.28.2") {
        Check::Available { version, root, .. } => {
            assert_eq!(version, "1.30.0");
            assert_eq!(root, new_root);
        }
        other => panic!("{other:?}"),
    }
    // The move sticks: a manifest at the new root that names no next_root
    // does not send the check after it back to the compiled-in root.
    match u.check("1.28.2") {
        Check::Available { root, .. } => assert_eq!(root, new_root),
        other => panic!("{other:?}"),
    }
    let requests = source.requests.lock().unwrap();
    assert_eq!(requests[0], format!("manifest {ROOT}"));
    assert_eq!(requests[1], format!("manifest {new_root}"));
    assert_eq!(requests[2], format!("manifest {new_root}"));
    assert_eq!(requests.len(), 3, "{requests:?}");
}

// ── the install, the swap, the previous, the rollback ─────────────────────

/// A fixture bundle: a directory with one file, its name the only thing the
/// swap looks at.
fn bundle_dir(root: &Path, name: &str, marker: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(dir.join("Contents/MacOS")).unwrap();
    std::fs::write(dir.join("Contents/MacOS/ainb-desktop"), marker).unwrap();
    dir
}

fn read_marker(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("Contents/MacOS/ainb-desktop")).unwrap()
}

#[test]
fn install_owner_is_read_from_the_running_executable() {
    let tmp = tempfile::tempdir().unwrap();
    let apps = tmp.path().join("Applications");
    let app = bundle_dir(&apps, "Agents in a Box.app", "v1");
    let exe = app.join("Contents/MacOS/ainb-desktop");
    // The install is the canonical path, so a symlinked Applications (or a
    // macOS temp dir under /private) resolves to one bundle.
    match Install::detect_from(&exe).unwrap() {
        Install::Bundle { app: found, .. } => {
            assert_eq!(found, std::fs::canonicalize(&app).unwrap());
        }
        other => panic!("{other:?}"),
    }

    let cellar = tmp.path().join("Cellar/x/1");
    let brewed = bundle_dir(&cellar, "Agents in a Box.app", "v1");
    let err = Install::detect_from(&brewed.join("Contents/MacOS/ainb-desktop")).unwrap_err();
    assert!(err.to_string().contains("Homebrew"), "{err}");

    let volumes = tmp.path().join("Volumes/Agents in a Box");
    let mounted = bundle_dir(&volumes, "Agents in a Box.app", "v1");
    let err = Install::detect_from(&mounted.join("Contents/MacOS/ainb-desktop")).unwrap_err();
    assert!(err.to_string().contains("Applications"), "{err}");

    let usr = tmp.path().join("usr/bin");
    std::fs::create_dir_all(&usr).unwrap();
    std::fs::write(usr.join("ainb-desktop"), "deb").unwrap();
    let err = Install::detect_from(&usr.join("ainb-desktop")).unwrap_err();
    assert!(err.to_string().contains("package manager"), "{err}");

    let plain = tmp.path().join("bin");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join("ainb-desktop.AppImage"), "img").unwrap();
    match Install::detect_from_appimage(&plain.join("ainb-desktop.AppImage")).unwrap() {
        Install::AppImage { file } => assert!(file.ends_with("ainb-desktop.AppImage")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn swap_keeps_the_previous_byte_for_byte_and_rollback_restores_it() {
    let tmp = tempfile::tempdir().unwrap();
    let apps = tmp.path().join("Applications");
    let app = bundle_dir(&apps, "Agents in a Box.app", "v1");
    let staged = bundle_dir(&tmp.path().join("staging"), "Agents in a Box.app", "v2");
    let install = Install::Bundle {
        app: app.clone(),
        previous: apps.join("Agents in a Box.app.previous"),
    };
    swap(&install, &staged).unwrap();
    assert_eq!(read_marker(&app), "v2");
    assert_eq!(
        read_marker(&apps.join("Agents in a Box.app.previous")),
        "v1"
    );
    assert!(!staged.exists(), "the staged bundle moved, not copied");

    rollback(&install).unwrap();
    assert_eq!(read_marker(&app), "v1");
    assert!(!apps.join("Agents in a Box.app.previous").exists());
}

#[test]
fn clear_previous_removes_only_the_previous() {
    let tmp = tempfile::tempdir().unwrap();
    let apps = tmp.path().join("Applications");
    let app = bundle_dir(&apps, "Agents in a Box.app", "v2");
    let previous = bundle_dir(&apps, "Agents in a Box.app.previous", "v1");
    let install = Install::Bundle {
        app: app.clone(),
        previous: previous.clone(),
    };
    clear_previous(&install).unwrap();
    assert!(!previous.exists());
    assert_eq!(read_marker(&app), "v2");
    // Idempotent when there is nothing to clear.
    clear_previous(&install).unwrap();
}

#[test]
fn an_interrupted_swap_is_finished_on_the_next_start() {
    let tmp = tempfile::tempdir().unwrap();
    let apps = tmp.path().join("Applications");
    // The state after a crash between the two renames: the previous exists,
    // the staged bundle exists beside it, and the app is gone.
    let previous = bundle_dir(&apps, "Agents in a Box.app.previous", "v1");
    let staged = bundle_dir(&apps, "Agents in a Box.app.next", "v2");
    let app = apps.join("Agents in a Box.app");
    assert!(!app.exists());
    let repaired = repair_interrupted_swap(&app).unwrap();
    assert!(repaired, "nothing was repaired");
    assert_eq!(read_marker(&app), "v2");
    assert!(
        previous.exists(),
        "the previous stays until the new app connects"
    );
    assert!(!staged.exists());
    // Nothing to do when the app is in place.
    assert!(!repair_interrupted_swap(&app).unwrap());
}

/// The startup repair from what the process knows about itself: the path of
/// the executable, and on Linux the `$APPIMAGE` the runtime mounted.
#[test]
fn the_startup_repair_reads_the_bundle_from_the_executable_and_the_appimage_from_its_env() {
    // A relaunch from the parked bundle after a crash between the renames.
    let tmp = tempfile::tempdir().unwrap();
    let apps = tmp.path().join("Applications");
    let previous = bundle_dir(&apps, "Agents in a Box.app.previous", "v1");
    bundle_dir(&apps, "Agents in a Box.app.next", "v2");
    let exe = previous.join("Contents/MacOS/ainb-desktop");
    assert!(repair_at_startup_from(&exe, None).unwrap());
    assert_eq!(read_marker(&apps.join("Agents in a Box.app")), "v2");

    // The same crash on Linux: the runtime mounted `<name>.AppImage.previous`
    // and set $APPIMAGE to it; the executable path is inside the mount.
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let parked = bin.join("ainb.AppImage.previous");
    std::fs::write(&parked, b"v1").unwrap();
    std::fs::write(bin.join("ainb.AppImage.next"), b"v2").unwrap();
    let mounted = tmp.path().join("mount/AppRun");
    assert!(repair_at_startup_from(&mounted, Some(&parked)).unwrap());
    assert_eq!(std::fs::read(bin.join("ainb.AppImage")).unwrap(), b"v2");
    assert!(!bin.join("ainb.AppImage.next").exists());
    assert!(parked.exists(), "the previous stays as the rollback slot");

    // A normal launch repairs nothing.
    assert!(!repair_at_startup_from(&mounted, Some(&bin.join("ainb.AppImage"))).unwrap());
    assert!(
        !repair_at_startup_from(&apps.join("Agents in a Box.app/Contents/MacOS/x"), None).unwrap()
    );
}

/// The whole apply path on a real disk image: a `.dmg` made with `hdiutil`
/// around a fixture app whose `Info.plist` names its version, served by the
/// fake source and hashed by the manifest; the app is mounted, copied out,
/// its version read back as data, and swapped in with the previous kept.
/// Checks 4, 5 and 7 accepted in one run.
#[cfg(target_os = "macos")]
#[test]
fn a_disk_image_is_mounted_copied_checked_and_swapped_in() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let apps = tmp.path().join("Applications");
    let installed = bundle_dir(&apps, "Agents in a Box.app", "v1");
    // The new version, as the release would ship it.
    let src = tmp.path().join("src");
    let new_app = bundle_dir(&src, "Agents in a Box.app", "v2");
    std::fs::write(
        new_app.join("Contents/Info.plist"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleShortVersionString</key><string>1.29.0</string>
<key>CFBundleIdentifier</key><string>dev.agentsinabox.desktop</string>
</dict></plist>
"#,
    )
    .unwrap();
    let dmg = tmp.path().join("ainb-desktop-1.29.0-x.dmg");
    let made = std::process::Command::new("hdiutil")
        .args([
            "create",
            "-quiet",
            "-volname",
            "Agents in a Box",
            "-format",
            "UDZO",
            "-srcfolder",
        ])
        .arg(&src)
        .arg(&dmg)
        .status()
        .unwrap();
    assert!(made.success(), "hdiutil create failed");
    let bytes = std::fs::read(&dmg).unwrap();

    let mut source = FakeSource::default();
    let archive = "ainb-desktop-1.29.0-x.dmg";
    source.manifests.insert(
        ROOT.into(),
        signed(&key(), &manifest("1.29.0", archive, &sha(&bytes), "")),
    );
    source.files.insert(format!("{ROOT}/{archive}"), bytes);
    let u = updater(Arc::new(source), Channel::Stable, &home);
    let check = u.check("1.28.2");
    assert!(matches!(check, Check::Available { .. }), "{check:?}");
    let install = Install::Bundle {
        app: installed.clone(),
        previous: apps.join("Agents in a Box.app.previous"),
    };
    let mut phases: Vec<Phase> = Vec::new();
    let swapped = u.apply_with(&check, &install, &mut |phase| phases.push(phase)).unwrap();
    assert_eq!(swapped, installed);
    assert_eq!(read_marker(&installed), "v2");
    assert_eq!(
        read_marker(&apps.join("Agents in a Box.app.previous")),
        "v1"
    );
    // The apply is framed: downloading (with progress), verifying, applying.
    assert!(
        matches!(phases.first(), Some(Phase::Downloading { received, total }) if *received > 0 && total.is_some()),
        "{phases:?}"
    );
    let kinds: Vec<&str> = phases
        .iter()
        .map(|p| match p {
            Phase::Downloading { .. } => "downloading",
            Phase::Verifying => "verifying",
            Phase::Applying => "applying",
            other => panic!("{other:?}"),
        })
        .collect();
    let mut dedup = kinds.clone();
    dedup.dedup();
    assert_eq!(
        dedup,
        ["downloading", "verifying", "applying"],
        "{phases:?}"
    );
    // One install in flight: a second apply before the restart is refused,
    // and the rollback slot still holds v1 (a second swap would have parked
    // v2 there).
    let again = u.apply(&check, &install).unwrap_err();
    assert!(again.to_string().contains("restart"), "{again:#}");
    assert_eq!(read_marker(&installed), "v2");
    assert_eq!(
        read_marker(&apps.join("Agents in a Box.app.previous")),
        "v1"
    );
    // Nothing is left beside the install but the app and its previous.
    let mut left: Vec<String> = std::fs::read_dir(&apps)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(
        left,
        vec!["Agents in a Box.app", "Agents in a Box.app.previous"]
    );
}

/// Check 5 declined: a disk image whose app says another version than the
/// manifest is refused and the install untouched.
#[cfg(target_os = "macos")]
#[test]
fn a_bundle_whose_own_version_disagrees_with_the_manifest_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let apps = tmp.path().join("Applications");
    let installed = bundle_dir(&apps, "Agents in a Box.app", "v1");
    let src = tmp.path().join("src");
    let new_app = bundle_dir(&src, "Agents in a Box.app", "v2");
    std::fs::write(
        new_app.join("Contents/Info.plist"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleShortVersionString</key><string>1.28.9</string>
</dict></plist>
"#,
    )
    .unwrap();
    let dmg = tmp.path().join("wrong.dmg");
    let made = std::process::Command::new("hdiutil")
        .args([
            "create",
            "-quiet",
            "-volname",
            "Agents in a Box",
            "-format",
            "UDZO",
            "-srcfolder",
        ])
        .arg(&src)
        .arg(&dmg)
        .status()
        .unwrap();
    assert!(made.success());
    let bytes = std::fs::read(&dmg).unwrap();
    let mut source = FakeSource::default();
    let archive = "ainb-desktop-1.29.0-x.dmg";
    source.manifests.insert(
        ROOT.into(),
        signed(&key(), &manifest("1.29.0", archive, &sha(&bytes), "")),
    );
    source.files.insert(format!("{ROOT}/{archive}"), bytes);
    let u = updater(Arc::new(source), Channel::Stable, &home);
    let check = u.check("1.28.2");
    let install = Install::Bundle {
        app: installed.clone(),
        previous: apps.join("Agents in a Box.app.previous"),
    };
    let err = u.apply(&check, &install).unwrap_err();
    assert!(err.to_string().contains("1.28.9"), "{err}");
    assert_eq!(read_marker(&installed), "v1");
    assert!(!apps.join("Agents in a Box.app.previous").exists());
    assert!(!apps.join("Agents in a Box.app.next").exists());
}

#[cfg(target_os = "macos")]
#[test]
fn a_staged_bundle_loses_its_quarantine_attribute_before_the_swap() {
    let tmp = tempfile::tempdir().unwrap();
    let apps = tmp.path().join("Applications");
    let app = bundle_dir(&apps, "Agents in a Box.app", "v1");
    let staged = bundle_dir(&tmp.path().join("staging"), "Agents in a Box.app", "v2");
    // On the bundle and on a file inside it: Gatekeeper reads the inner
    // files too, so a top-level strip alone is not a strip.
    for tagged in [staged.clone(), staged.join("Contents/MacOS/ainb-desktop")] {
        let status = std::process::Command::new("xattr")
            .args(["-w", "com.apple.quarantine", "0083;00000000;test;"])
            .arg(&tagged)
            .status()
            .unwrap();
        assert!(status.success());
    }
    let install = Install::Bundle {
        app: app.clone(),
        previous: apps.join("Agents in a Box.app.previous"),
    };
    swap(&install, &staged).unwrap();
    let out = std::process::Command::new("xattr").arg("-lr").arg(&app).output().unwrap();
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("com.apple.quarantine"),
        "quarantine survived the swap"
    );
}
