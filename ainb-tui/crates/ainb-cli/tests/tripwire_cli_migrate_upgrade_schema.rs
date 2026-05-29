//! Tripwire — bead v12.C.5.
//!
//! `ainb migrate --upgrade-schema` seeds every source whose
//! `target_layout` is empty with the canonical
//! [`BOOTSTRAP_DEFAULT_MAPPINGS`] entries so it round-trips through
//! `MappingEngine::resolve_pair` identically to an explicit layout.
//!
//! Scenarios:
//!   - Source with `target_layout: []` → manifest after has the
//!     bootstrap-default entries populated verbatim.
//!   - Re-running is a no-op: the second run reports "0 sources
//!     updated" and the file bytes don't change.
//!   - A source with an already-populated `target_layout` is left
//!     alone (never overwritten — schema upgrade only **fills in**,
//!     never replaces a user-authored layout).

use std::path::Path;

use ainb_cli::{Command, MigrateArgs, dispatch};
use ainb_skill_core::manifest::{Manifest, SourceEntry, TargetMapping};
use ainb_skill_core::mapping::bootstrap_default_mappings;
use ainb_skill_core::paths::manifest_path_in;

fn tmp_home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ainb-tripwire-migrate-upgrade-schema-")
        .tempdir()
        .expect("tempdir")
}

fn run_migrate(home: &Path, args: MigrateArgs) -> (String, anyhow::Result<()>) {
    let mut buf = Vec::new();
    let res = dispatch(home, Command::Migrate(args), &mut buf);
    (String::from_utf8(buf).expect("utf8"), res)
}

fn upgrade_schema_args() -> MigrateArgs {
    MigrateArgs {
        upgrade_schema: true,
        ..MigrateArgs::default()
    }
}

fn seed_manifest_with_empty_layout_source(home: &Path, source_name: &str) {
    let manifest = Manifest {
        sources: vec![SourceEntry {
            name: source_name.to_string(),
            kind: Some("raw".into()),
            uri: "local:/fake".into(),
            r#ref: "main".into(),
            enabled: true,
            read_only: false,
            target_layout: Vec::new(),
        }],
        ..Manifest::default()
    };
    manifest.save_to(&manifest_path_in(home)).expect("save manifest");
}

#[test]
fn upgrade_schema_populates_empty_target_layout_with_bootstrap_defaults() {
    let home = tmp_home();
    seed_manifest_with_empty_layout_source(home.path(), "fixture");

    let (out, res) = run_migrate(home.path(), upgrade_schema_args());
    res.expect("upgrade-schema ok");

    let manifest = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
    let src = &manifest.sources[0];
    let expected: Vec<TargetMapping> = bootstrap_default_mappings();
    assert_eq!(
        src.target_layout, expected,
        "every BOOTSTRAP_DEFAULT_MAPPINGS entry should land in the source's target_layout verbatim"
    );

    // Output reports the per-source count + total.
    assert!(
        out.contains("fixture"),
        "expected per-source line for `fixture`: {out}"
    );
    assert!(
        out.contains(&format!("{}", expected.len())),
        "expected the mapping count to appear in output: {out}"
    );
}

#[test]
fn upgrade_schema_is_idempotent_second_run_no_op() {
    let home = tmp_home();
    seed_manifest_with_empty_layout_source(home.path(), "fixture-idem");

    // First run: populates.
    let (_out1, res1) = run_migrate(home.path(), upgrade_schema_args());
    res1.expect("first upgrade-schema ok");
    let bytes_after_first = std::fs::read(manifest_path_in(home.path())).unwrap();

    // Second run: no-op.
    let (out2, res2) = run_migrate(home.path(), upgrade_schema_args());
    res2.expect("second upgrade-schema ok");
    let bytes_after_second = std::fs::read(manifest_path_in(home.path())).unwrap();

    assert_eq!(
        bytes_after_first, bytes_after_second,
        "second run must not mutate the manifest bytes — upgrade-schema is idempotent"
    );
    // "0 sources updated" or equivalent — accept any of a few likely wordings
    // but the literal "0" must appear so a regression that double-applies
    // the bootstrap defaults shows up immediately.
    assert!(
        out2.contains("0"),
        "second run output should advertise 0 sources updated: {out2}"
    );
}

#[test]
fn upgrade_schema_leaves_existing_layout_alone() {
    // A source that already has a target_layout (any non-empty layout)
    // must NOT be overwritten. upgrade-schema only fills in empty slots
    // so user-authored mappings are preserved.
    let home = tmp_home();
    let preexisting = TargetMapping {
        glob: "custom/**".to_string(),
        home: std::path::PathBuf::from(".custom"),
        repo: std::path::PathBuf::from("custom"),
    };
    let manifest = Manifest {
        sources: vec![SourceEntry {
            name: "already-mapped".to_string(),
            kind: Some("raw".into()),
            uri: "local:/x".into(),
            r#ref: "main".into(),
            enabled: true,
            read_only: false,
            target_layout: vec![preexisting.clone()],
        }],
        ..Manifest::default()
    };
    manifest.save_to(&manifest_path_in(home.path())).unwrap();

    let (_out, res) = run_migrate(home.path(), upgrade_schema_args());
    res.expect("upgrade-schema ok");

    let after = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
    assert_eq!(
        after.sources[0].target_layout,
        vec![preexisting],
        "must not overwrite an existing target_layout"
    );
}
