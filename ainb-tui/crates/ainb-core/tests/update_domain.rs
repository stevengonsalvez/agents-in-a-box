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
