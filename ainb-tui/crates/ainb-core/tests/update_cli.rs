//! Public clap surface for `ainb update`.

use ainb::cli::{registry::CommandRegistry, root_clap_command};

#[test]
fn update_cli_exposes_apply_check_status_and_schedule() {
    let registry = CommandRegistry::built_ins();
    let app = registry.build_clap(root_clap_command());

    for argv in [
        vec!["ainb", "update"],
        vec!["ainb", "update", "--yes"],
        vec!["ainb", "upgrade", "--yes"],
        vec!["ainb", "update", "check", "--scheduled"],
        vec!["ainb", "update", "status", "--format", "json"],
        vec!["ainb", "update", "schedule", "enable"],
    ] {
        app.clone().try_get_matches_from(argv).unwrap();
    }

    assert!(registry.find("upgrade").is_some());
    let alias_matches = app.clone().try_get_matches_from(["ainb", "upgrade", "--yes"]).unwrap();
    assert_eq!(alias_matches.subcommand_name(), Some("update"));
}
