//! Public clap surface for `ainb update`.

use ainb::cli::{registry::CommandRegistry, root_clap_command};

#[test]
fn update_cli_exposes_apply_check_status_and_schedule() {
    let app = CommandRegistry::built_ins().build_clap(root_clap_command());

    for argv in [
        vec!["ainb", "update"],
        vec!["ainb", "update", "--yes"],
        vec!["ainb", "update", "check", "--scheduled"],
        vec!["ainb", "update", "status", "--format", "json"],
        vec!["ainb", "update", "schedule", "enable"],
    ] {
        app.clone().try_get_matches_from(argv).unwrap();
    }
}
