//! Domain behavior for signed Ainb releases and daily update schedules.

use ainb::cli::update::{
    ReleaseManifest, ReleaseState, UpdateAvailability, UpdateSchedule, verify_manifest_with_key,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signer, SigningKey};

#[test]
fn stable_manifest_newer_than_local_is_available() {
    let manifest = ReleaseManifest::for_test("1.23.0");
    let state = ReleaseState::from_manifest("1.22.5", &manifest, 1_700_000_000_000).unwrap();

    assert_eq!(state.availability, UpdateAvailability::Available);
    assert_eq!(state.available_version.as_deref(), Some("1.23.0"));
}

#[test]
fn prerelease_manifest_is_rejected() {
    let manifest = ReleaseManifest::for_test("1.23.0-beta.1");

    assert!(ReleaseState::from_manifest("1.22.5", &manifest, 1_700_000_000_000).is_err());
}

#[test]
fn signed_manifest_requires_the_matching_ed25519_public_key() {
    let signing_key = SigningKey::from_bytes(&[7; 32]);
    let bytes = br#"{"version":"1.23.0","assets":[]}"#;
    let signature = signing_key.sign(bytes);
    let verified = verify_manifest_with_key(
        bytes,
        &STANDARD.encode(signature.to_bytes()),
        &STANDARD.encode(signing_key.verifying_key().as_bytes()),
    )
    .unwrap();

    assert_eq!(verified.version, "1.23.0");
    assert!(
        verify_manifest_with_key(
            bytes,
            &STANDARD.encode(signature.to_bytes()),
            &STANDARD.encode([8; 32])
        )
        .is_err()
    );
}

#[test]
fn signed_manifest_rejects_unsafe_asset_metadata() {
    let signing_key = SigningKey::from_bytes(&[9; 32]);
    let bytes = br#"{"version":"1.23.0","assets":[{"target":"x86_64-unknown-linux-gnu","archive":"../ainb.tar.gz","sha256":"0000000000000000000000000000000000000000000000000000000000000000"}]}"#;
    let signature = signing_key.sign(bytes);

    assert!(
        verify_manifest_with_key(
            bytes,
            &STANDARD.encode(signature.to_bytes()),
            &STANDARD.encode(signing_key.verifying_key().as_bytes()),
        )
        .is_err()
    );
}

/// `ReleaseManifest` and `ReleaseAsset` exactly as they shipped in 1.28.2,
/// copied rather than imported so the structs in `update.rs` growing can never
/// make these two tests pass by definition. This is the shape every CLI in the
/// field decodes the manifest with.
mod shipped_1_28_2 {
    #[derive(Debug, serde::Deserialize)]
    pub struct ReleaseManifest {
        pub version: String,
        #[serde(default)]
        pub assets: Vec<ReleaseAsset>,
    }

    #[derive(Debug, serde::Deserialize)]
    pub struct ReleaseAsset {
        pub target: String,
        pub archive: String,
        pub sha256: String,
    }

    impl ReleaseManifest {
        /// `asset_for_current_platform` as shipped: the FIRST entry whose
        /// `target` matches, and nothing else about it is looked at.
        pub fn asset_for(&self, target: &str) -> Option<&ReleaseAsset> {
            self.assets.iter().find(|asset| asset.target == target)
        }
    }
}

const TARGETS: [&str; 3] = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
];

/// The manifest the release workflow writes since the desktop shipped: the
/// CLI archives under `assets[]` and the desktop bundles under `desktop`.
fn manifest_with_desktop_key() -> String {
    let sha = "0".repeat(64);
    let assets: Vec<String> = TARGETS
        .iter()
        .map(|t| {
            format!(r#"{{"target":"{t}","archive":"ainb-1.29.0-{t}.tar.gz","sha256":"{sha}"}}"#)
        })
        .collect();
    let desktop = [
        ("aarch64-apple-darwin", "dmg", "dmg"),
        ("x86_64-apple-darwin", "dmg", "dmg"),
        ("x86_64-unknown-linux-gnu", "appimage", "AppImage"),
        ("x86_64-unknown-linux-gnu", "deb", "deb"),
    ]
    .iter()
    .map(|(t, format, ext)| {
        format!(
            r#"{{"target":"{t}","format":"{format}","archive":"ainb-desktop-1.29.0-{t}.{ext}","sha256":"{sha}","signed":false}}"#
        )
    })
    .collect::<Vec<_>>();
    format!(
        r#"{{"version":"1.29.0","assets":[{}],"desktop":[{}]}}"#,
        assets.join(","),
        desktop.join(",")
    )
}

/// Criterion 8 of the D4-prime goal: a CLI shipped before the desktop reads
/// the new manifest and installs only its own archive, for every target the
/// matrix builds. The key is ignored by the shipped struct, and the pinned
/// public key still covers the whole file.
#[test]
fn a_shipped_cli_reads_the_desktop_key_manifest_and_picks_its_own_archive() {
    let bytes = manifest_with_desktop_key();
    let shipped: shipped_1_28_2::ReleaseManifest =
        serde_json::from_str(&bytes).expect("the shipped struct ignores the desktop key");
    assert_eq!(shipped.version, "1.29.0");
    for target in TARGETS {
        let asset = shipped.asset_for(target).expect("an archive for this target");
        assert_eq!(asset.archive, format!("ainb-1.29.0-{target}.tar.gz"));
        assert!(!asset.archive.ends_with(".dmg") && !asset.archive.ends_with(".AppImage"));
    }

    // The current struct decodes it too, through the same verify path.
    let signing_key = SigningKey::from_bytes(&[11; 32]);
    let signature = signing_key.sign(bytes.as_bytes());
    let verified = verify_manifest_with_key(
        bytes.as_bytes(),
        &STANDARD.encode(signature.to_bytes()),
        &STANDARD.encode(signing_key.verifying_key().as_bytes()),
    )
    .expect("the current struct accepts the desktop key");
    assert_eq!(verified.assets.len(), 3);
}

/// The rule that stops the failure below at the source: the CURRENT verifier
/// refuses any `assets[]` entry that is not a CLI archive (`ainb-` prefix,
/// `.tar.gz` suffix), so a manifest with a desktop bundle under `assets[]`
/// never verifies, whichever key signed it.
#[test]
fn a_desktop_bundle_under_assets_is_refused_by_the_current_verifier() {
    let signing_key = SigningKey::from_bytes(&[16; 32]);
    let sha = "0".repeat(64);
    let target = "aarch64-apple-darwin";
    for archive in [
        format!("ainb-desktop-1.29.0-{target}.dmg"),
        format!("ainb-desktop-1.29.0-{target}.AppImage"),
        format!("ainb-1.29.0-{target}.zip"),
        format!("notainb-1.29.0-{target}.tar.gz"),
    ] {
        let bytes = format!(
            r#"{{"version":"1.29.0","assets":[{{"target":"{target}","archive":"{archive}","sha256":"{sha}"}}]}}"#
        );
        let signature = signing_key.sign(bytes.as_bytes());
        assert!(
            verify_manifest_with_key(
                bytes.as_bytes(),
                &STANDARD.encode(signature.to_bytes()),
                &STANDARD.encode(signing_key.verifying_key().as_bytes()),
            )
            .is_err(),
            "{archive} was accepted under assets[]"
        );
    }
    // The CLI archive shape still verifies.
    let bytes = format!(
        r#"{{"version":"1.29.0","assets":[{{"target":"{target}","archive":"ainb-1.29.0-{target}.tar.gz","sha256":"{sha}"}}]}}"#
    );
    let signature = signing_key.sign(bytes.as_bytes());
    assert!(
        verify_manifest_with_key(
            bytes.as_bytes(),
            &STANDARD.encode(signature.to_bytes()),
            &STANDARD.encode(signing_key.verifying_key().as_bytes()),
        )
        .is_ok()
    );
}

/// The failure the `desktop` key exists to prevent, kept in the suite as the
/// reason: the same bundles placed in `assets[]` ahead of the CLI archives
/// would hand a SHIPPED CLI (1.28.2, no such rule) a `.dmg` for its target,
/// because it matches on target alone and takes the first hit.
#[test]
fn desktop_bundles_inside_assets_would_be_installed_by_a_shipped_cli() {
    let sha = "0".repeat(64);
    let target = "aarch64-apple-darwin";
    let bytes = format!(
        r#"{{"version":"1.29.0","assets":[
            {{"target":"{target}","archive":"ainb-desktop-1.29.0-{target}.dmg","sha256":"{sha}"}},
            {{"target":"{target}","archive":"ainb-1.29.0-{target}.tar.gz","sha256":"{sha}"}}
        ]}}"#
    );
    let shipped: shipped_1_28_2::ReleaseManifest = serde_json::from_str(&bytes).unwrap();
    let picked = shipped.asset_for(target).unwrap();
    assert!(
        picked.archive.ends_with(".dmg"),
        "a shipped CLI takes the first target hit: {}",
        picked.archive
    );
}

#[test]
fn local_newer_than_manifest_never_downgrades() {
    let manifest = ReleaseManifest::for_test("1.22.5");
    let state = ReleaseState::from_manifest("1.23.0", &manifest, 1_700_000_000_000).unwrap();

    assert_eq!(state.availability, UpdateAvailability::CurrentOrNewer);
    assert!(state.available_version.is_none());
}

#[test]
fn update_state_round_trips_from_its_own_atomic_file() {
    let temp = tempfile::tempdir().unwrap();
    let manifest = ReleaseManifest::for_test("1.23.0");
    let state = ReleaseState::from_manifest("1.22.5", &manifest, 1_700_000_000_000).unwrap();
    let path = temp.path().join("update-state.json");

    state.save_to(&path).unwrap();

    assert_eq!(ReleaseState::load_from(&path).unwrap(), state);
}

#[test]
fn macos_schedule_runs_background_check_daily() {
    let plist = UpdateSchedule::daily().launchd_plist("ainb");

    assert!(plist.contains("<integer>86400</integer>"));
    assert!(plist.contains("ainb update check --scheduled"));
    assert!(plist.contains("<key>RunAtLoad</key>"));
}

#[test]
fn linux_schedule_is_persistent_daily_timer() {
    let timer = UpdateSchedule::daily().systemd_timer();
    let service = UpdateSchedule::daily().systemd_service("ainb");

    assert!(timer.contains("OnUnitActiveSec=86400"));
    assert!(timer.contains("Persistent=true"));
    assert!(service.contains("ainb update check --scheduled"));
}
