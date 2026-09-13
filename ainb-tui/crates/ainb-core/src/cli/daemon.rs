// ABOUTME: `ainb daemon` lives in `ainb_app::cli::daemon`, next to the daemons
// screen that drives the same actions. This shim re-exports it and keeps the
// test that parses its delegated argvs through the full clap tree, which only
// this crate builds.

pub use ainb_app::cli::daemon::*;

#[cfg(test)]
mod tests {
    /// Every argv this module delegates to must actually parse against the real
    /// clap tree. `fleet atc teardown --yes` did not (there is no such flag),
    /// so `ainb daemon atc stop`, one of the three verbs this whole surface
    /// exists to provide, failed with a usage error every single time.
    #[test]
    fn every_delegated_argv_parses_against_the_real_cli() {
        let registry = crate::cli::registry::CommandRegistry::built_ins();
        let app = registry.build_clap(clap::Command::new("ainb"));
        // The argvs `control` can produce. ATC's carry a placeholder instance
        // name; the shape is what is under test, not the name.
        let delegated: &[&[&str]] = &[
            &["hangar", "daemon", "start"],
            &["hangar", "daemon", "stop"],
            &["hangar", "daemon", "restart"],
            &["notifyd", "restart"],
            &["notifyd", "stop"],
            &["fleet", "atc", "repair", "main"],
            // Start on a dead session respawns it, passing the instance's own
            // settings back so nothing is reconfigured.
            &[
                "fleet",
                "atc",
                "setup",
                "main",
                "--interval",
                "10",
                "--idle-pause",
                "60",
            ],
            &[
                "fleet",
                "atc",
                "setup",
                "main",
                "--interval",
                "10",
                "--idle-pause",
                "60",
                "--no-heartbeat",
            ],
            &["fleet", "atc", "teardown", "main"],
            &["fleet", "bridge", "install"],
            &["fleet", "bridge", "uninstall"],
            &["fleet", "daemon"],
        ];
        for argv in delegated {
            let full: Vec<&str> = std::iter::once("ainb").chain(argv.iter().copied()).collect();
            if let Err(e) = app.clone().try_get_matches_from(&full) {
                panic!("`ainb {}` does not parse: {e}", argv.join(" "));
            }
        }
    }
}
