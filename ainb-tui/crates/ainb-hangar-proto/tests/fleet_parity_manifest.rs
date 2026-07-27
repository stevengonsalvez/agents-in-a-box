//! Fleet parity manifest schema and protocol catalogue gate.

use std::{
    fs,
    path::{Path, PathBuf},
};

use ainb_hangar_proto::{fleet::FLEET_PROTOCOL_CAPABILITY_IDS, methods};
use serde::Deserialize;

const MANIFEST_PATH: &str = "fleet-parity/manifest.json";
const SURFACE_STATUSES: &[&str] = &[
    "pass",
    "known_gap",
    "blocked_proof",
    "not_applicable",
    "deferred",
];
const RELEASE_CLASSIFICATIONS: &[&str] = &[
    "pass",
    "known_tui_gap",
    "known_swift_gap",
    "daemon_gap",
    "blocked_proof",
    "deferred_tui",
];

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    phase: String,
    #[serde(default)]
    tui_deferral: Option<TuiDeferral>,
    capabilities: Vec<Capability>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TuiDeferral {
    bead: String,
    gaps: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Capability {
    id: String,
    tier: String,
    daemon_request_method: String,
    #[serde(default)]
    daemon_notification_method: Option<String>,
    expected_behavior: String,
    daemon: Surface,
    tui: Surface,
    swift: Surface,
    release_classification: String,
    #[serde(default)]
    gap_bead: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Surface {
    status: String,
    evidence_paths: Vec<String>,
    #[serde(default)]
    evidence_root: Option<String>,
    #[serde(default)]
    rationale: Option<String>,
}

fn ainb_tui_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("ainb-tui root exists")
}

fn workspace_root() -> PathBuf {
    ainb_tui_root().parent().expect("workspace root exists").to_path_buf()
}

fn manifest() -> Manifest {
    let bytes =
        fs::read(ainb_tui_root().join("fleet-parity/manifest.json")).expect("manifest exists");
    serde_json::from_slice(&bytes).expect("manifest decodes")
}

fn surface_statuses(capability: &Capability) -> [&str; 3] {
    [
        &capability.daemon.status,
        &capability.tui.status,
        &capability.swift.status,
    ]
}

fn validate(manifest: &Manifest, root: &Path) -> Result<(), String> {
    let workspace = workspace_root();
    if manifest.schema_version != 1 {
        return Err("schema_version must be 1".to_string());
    }
    if !matches!(manifest.phase.as_str(), "e01_in_progress" | "e01_exit") {
        return Err("phase must be e01_in_progress or e01_exit".to_string());
    }

    let ids: Vec<_> =
        manifest.capabilities.iter().map(|capability| capability.id.as_str()).collect();
    let mut sorted_ids = ids.clone();
    sorted_ids.sort_unstable();
    sorted_ids.dedup();
    if ids != sorted_ids {
        return Err("capability ids must be unique and sorted".to_string());
    }
    if ids != FLEET_PROTOCOL_CAPABILITY_IDS {
        return Err("manifest capability ids must equal negotiation catalogue".to_string());
    }

    let any_tui_deferred = manifest
        .capabilities
        .iter()
        .any(|capability| capability.tui.status == "deferred");
    if any_tui_deferred
        && !manifest
            .capabilities
            .iter()
            .all(|capability| capability.tui.status == "deferred")
    {
        return Err("TUI deferral must cover every TUI surface".to_string());
    }
    match (any_tui_deferred, manifest.tui_deferral.as_ref()) {
        (true, Some(deferral))
            if !deferral.bead.trim().is_empty()
                && !deferral.gaps.is_empty()
                && deferral.gaps.iter().all(|gap| !gap.trim().is_empty()) => {}
        (true, _) => return Err("TUI deferral requires one bead and individual gaps".to_string()),
        (false, Some(_)) => {
            return Err("TUI deferral metadata requires deferred TUI surfaces".to_string());
        }
        (false, None) => {}
    }

    for capability in &manifest.capabilities {
        if !matches!(capability.tier.as_str(), "foundation" | "v1" | "v2") {
            return Err(format!("unknown tier for {}", capability.id));
        }
        if capability.expected_behavior.trim().is_empty() {
            return Err(format!("expected behavior missing for {}", capability.id));
        }
        if !methods::ALL_METHODS.contains(&capability.daemon_request_method.as_str()) {
            return Err(format!(
                "unknown daemon request method for {}",
                capability.id
            ));
        }
        if let Some(notification) = capability.daemon_notification_method.as_deref() {
            if !methods::FLEET_PROTOCOL_NOTIFICATION_METHODS.contains(&notification) {
                return Err(format!(
                    "unknown daemon notification method for {}",
                    capability.id
                ));
            }
            if methods::ALL_METHODS.contains(&notification) {
                return Err(format!(
                    "notification is also a request method for {}",
                    capability.id
                ));
            }
        }
        if !RELEASE_CLASSIFICATIONS.contains(&capability.release_classification.as_str()) {
            return Err(format!(
                "unknown release classification for {}",
                capability.id
            ));
        }

        let statuses = surface_statuses(capability);
        if statuses.iter().any(|status| !SURFACE_STATUSES.contains(status)) {
            return Err(format!("unknown surface status for {}", capability.id));
        }
        if capability.daemon.status == "deferred" || capability.swift.status == "deferred" {
            return Err(format!("only TUI may be deferred for {}", capability.id));
        }
        if statuses.iter().any(|status| *status == "known_gap")
            && capability.gap_bead.as_deref().unwrap_or_default().is_empty()
        {
            return Err(format!(
                "known gap requires separate-session bead for {}",
                capability.id
            ));
        }
        if matches!(
            capability.release_classification.as_str(),
            "known_tui_gap" | "known_swift_gap"
        ) && capability.gap_bead.as_deref().unwrap_or_default().is_empty()
        {
            return Err(format!(
                "release gap requires separate-session bead for {}",
                capability.id
            ));
        }
        for surface in [&capability.daemon, &capability.tui, &capability.swift] {
            if surface.status == "not_applicable"
                && surface.rationale.as_deref().unwrap_or_default().trim().is_empty()
            {
                return Err(format!(
                    "not_applicable requires rationale for {}",
                    capability.id
                ));
            }
            if surface.status == "pass" {
                if surface.evidence_paths.is_empty() {
                    return Err(format!(
                        "pass requires evidence paths for {}",
                        capability.id
                    ));
                }
                let evidence_root = match surface.evidence_root.as_deref().unwrap_or("ainb_tui") {
                    "ainb_tui" => root,
                    "workspace" => &workspace,
                    _ => return Err(format!("unknown evidence root for {}", capability.id)),
                };
                for evidence_path in &surface.evidence_paths {
                    let relative = Path::new(evidence_path);
                    if relative.is_absolute()
                        || relative.components().any(|part| part.as_os_str() == "..")
                    {
                        return Err(format!("invalid evidence path for {}", capability.id));
                    }
                    if !evidence_root.join(relative).exists() {
                        return Err(format!("missing evidence path for {}", capability.id));
                    }
                }
            }
        }
        if manifest.phase == "e01_exit"
            && capability.tier == "foundation"
            && (capability.daemon.status == "blocked_proof"
                || capability.swift.status == "blocked_proof")
        {
            return Err(format!(
                "Foundation daemon or Swift proof remains blocked at E01 exit for {}",
                capability.id
            ));
        }
        if manifest.phase == "e01_exit"
            && capability.tier == "foundation"
            && capability.tui.status == "blocked_proof"
        {
            return Err(format!(
                "Foundation TUI proof must be explicitly deferred or classified at E01 exit for {}",
                capability.id
            ));
        }
        if manifest.phase == "e01_exit"
            && capability.tier == "foundation"
            && capability.release_classification == "blocked_proof"
        {
            return Err(format!(
                "Foundation release classification remains blocked at E01 exit for {}",
                capability.id
            ));
        }
        if capability.release_classification == "deferred_tui"
            && capability.tui.status != "deferred"
        {
            return Err(format!(
                "deferred TUI release classification requires TUI deferral for {}",
                capability.id
            ));
        }
        if manifest.phase == "e01_exit"
            && capability.tier == "foundation"
            && ![&capability.daemon, &capability.tui, &capability.swift]
                .into_iter()
                .any(|surface| surface.status == "pass")
        {
            return Err(format!(
                "Foundation proof is missing at E01 exit for {}",
                capability.id
            ));
        }
    }
    Ok(())
}

#[test]
fn fleet_parity_manifest_is_valid() {
    validate(&manifest(), &ainb_tui_root()).expect("manifest is valid");
}

#[test]
fn manifest_capabilities_match_negotiation_catalogue() {
    let ids: Vec<_> = manifest().capabilities.into_iter().map(|capability| capability.id).collect();
    assert_eq!(ids, FLEET_PROTOCOL_CAPABILITY_IDS);
}

#[test]
fn manifest_rejects_unknown_capability_id() {
    let mut value = manifest();
    value.capabilities[0].id = "fleet.unknown".to_string();
    assert!(validate(&value, &ainb_tui_root()).is_err());
}

#[test]
fn manifest_rejects_unknown_notification_method() {
    let mut value = manifest();
    value.capabilities[0].daemon_notification_method = Some("fleet/not-real".to_string());
    assert!(validate(&value, &ainb_tui_root()).is_err());
}

#[test]
fn manifest_rejects_notification_as_request_method() {
    let mut value = manifest();
    value.capabilities[0].daemon_request_method = "fleet/resync_required".to_string();
    assert!(validate(&value, &ainb_tui_root()).is_err());
}

#[test]
fn known_gap_requires_separate_session_bead() {
    let mut value = manifest();
    value.capabilities[0].swift.status = "known_gap".to_string();
    assert!(validate(&value, &ainb_tui_root()).is_err());
}

#[test]
fn e01_exit_rejects_foundation_blocked_release_classification() {
    let mut value = foundation_exit_manifest();
    value
        .capabilities
        .iter_mut()
        .find(|capability| capability.tier == "foundation")
        .expect("Foundation capability")
        .release_classification = "blocked_proof".to_string();
    assert!(validate(&value, &ainb_tui_root()).is_err());
}

fn foundation_exit_manifest() -> Manifest {
    let mut value = manifest();
    value.phase = "e01_exit".to_string();
    for capability in &mut value.capabilities {
        if capability.tier == "foundation" {
            capability.release_classification = "deferred_tui".to_string();
            for surface in [&mut capability.daemon, &mut capability.swift] {
                surface.status = "pass".to_string();
                surface.evidence_paths = vec!["crates/ainb-hangar-proto/src/fleet.rs".to_string()];
                surface.evidence_root = None;
                surface.rationale = None;
            }
            capability.tui.status = "deferred".to_string();
            capability.tui.evidence_paths.clear();
            capability.tui.evidence_root = None;
            capability.tui.rationale = Some("Deferred under the TUI parity bead.".to_string());
        }
    }
    value
}

#[test]
fn e01_exit_rejects_foundation_capability_without_any_surface_proof() {
    let mut value = foundation_exit_manifest();
    let capability = value
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == "fleet.protocol.negotiate")
        .expect("negotiate capability");
    for surface in [
        &mut capability.daemon,
        &mut capability.tui,
        &mut capability.swift,
    ] {
        surface.status = "not_applicable".to_string();
        surface.evidence_paths.clear();
        surface.rationale = Some("No surface owns this capability.".to_string());
    }

    assert!(validate(&value, &ainb_tui_root()).is_err());
}

#[test]
fn e01_exit_allows_explicit_tui_deferral_when_daemon_and_swift_are_proven() {
    validate(&foundation_exit_manifest(), &ainb_tui_root())
        .expect("TUI deferral remains valid when daemon and Swift are proven");
}

#[test]
fn deferred_tui_requires_one_bead_and_individual_gaps() {
    let mut value = manifest();
    value.tui_deferral = None;
    assert!(validate(&value, &ainb_tui_root()).is_err());

    let mut value = manifest();
    value.tui_deferral.as_mut().expect("TUI deferral").gaps.clear();
    assert!(validate(&value, &ainb_tui_root()).is_err());
}

#[test]
fn e01_exit_rejects_daemon_or_swift_blocked_proof() {
    let mut daemon_blocked = foundation_exit_manifest();
    daemon_blocked
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == "fleet.protocol.negotiate")
        .expect("negotiate capability")
        .daemon
        .status = "blocked_proof".to_string();
    assert!(validate(&daemon_blocked, &ainb_tui_root()).is_err());

    let mut swift_blocked = foundation_exit_manifest();
    swift_blocked
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == "fleet.protocol.negotiate")
        .expect("negotiate capability")
        .swift
        .status = "blocked_proof".to_string();
    assert!(validate(&swift_blocked, &ainb_tui_root()).is_err());
}

#[test]
fn manifest_path_is_checked_in() {
    assert!(ainb_tui_root().join(MANIFEST_PATH).exists());
}
