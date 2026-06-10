//! CLI-surface tests for `ainb hangar beads reconcile` (P2.5).
//!
//! These cover the clap parse layer only — the `beads` subgroup accepts the
//! `reconcile` verb with `--dry-run`, repeated `--label`, and `--format json`,
//! and the parsed options round-trip into [`ReconcileOpts`]. The full command
//! path `ainb hangar beads reconcile` is exercised by [`HangarBeadsCli`], the
//! shape the host `ainb hangar` tree plugs in. The end-to-end behaviour of the
//! service is covered in `reconcile.rs`.

use clap::Parser;

use ainb_hangar_daemon::beads_sync::reconcile::cli::{
    BeadsCli, BeadsCommand, HangarBeadsCli, OutputFormat,
};

/// Parse the `beads` subgroup in isolation (argv excludes the `beads` token —
/// the parent `ainb hangar` tree consumes it).
fn parse(args: &[&str]) -> BeadsCli {
    let mut full = vec!["beads"];
    full.extend_from_slice(args);
    BeadsCli::try_parse_from(full).expect("clap parse")
}

#[test]
fn test_cli_invocation() {
    let cli = parse(&["reconcile", "--dry-run", "--format", "json"]);
    let BeadsCommand::Reconcile(args) = cli.command;
    assert!(args.dry_run);
    assert_eq!(args.format, OutputFormat::Json);
}

#[test]
fn test_cli_full_command_path() {
    // The full `... beads reconcile ...` path as the host tree presents it.
    let cli = HangarBeadsCli::try_parse_from(["ainb-hangar", "beads", "reconcile", "--dry-run"])
        .expect("clap parse");
    let BeadsCommand::Reconcile(args) = &cli.beads().command;
    assert!(args.dry_run);
}

#[test]
fn test_cli_defaults_are_text_and_apply() {
    let cli = parse(&["reconcile"]);
    let BeadsCommand::Reconcile(args) = cli.command;
    assert!(!args.dry_run, "default is apply, not dry-run");
    assert_eq!(args.format, OutputFormat::Text);
    assert!(args.label.is_empty());
}

#[test]
fn test_cli_repeated_labels_collect() {
    let cli = parse(&["reconcile", "--label", "foo", "--label", "bar"]);
    let BeadsCommand::Reconcile(args) = cli.command;
    assert_eq!(args.label, vec!["foo".to_string(), "bar".to_string()]);
}

#[test]
fn test_cli_args_map_to_reconcile_opts() {
    let cli = parse(&["reconcile", "--dry-run", "--label", "foo"]);
    let BeadsCommand::Reconcile(args) = cli.command;
    let opts = args.to_opts("ws-1");
    assert!(opts.dry_run);
    assert_eq!(opts.label_filter, vec!["foo".to_string()]);
    assert_eq!(opts.workspace_id, "ws-1");
}
