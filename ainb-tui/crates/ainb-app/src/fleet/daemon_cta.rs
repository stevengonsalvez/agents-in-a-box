// ABOUTME: The Pal pane's offer to start the hangar daemon it needs, and
// what that start actually reported.
//
// The pane it sits on can only fail one way that an operator can fix from the
// keyboard: the daemon behind every one of its four calls is not running. Every
// other failure is the daemon's own words, and this offers nothing for those —
// a key that starts a second daemon while the first is merely slow is a worse
// surface than the error it replaced.
//
// It owns no lifecycle of its own. The start is the SAME
// `Effect::RunDaemonAction` the Daemons screen queues, run and reported by the
// same host code, so the two cannot drift in what they run or in what they
// report having run.

use crate::components::daemons::ActionOutcome;

/// The stable id of the daemon this offer starts, as `ainb daemon <id>` spells
/// it.
///
/// Read off [`crate::fleet::daemons::probe::DaemonKind`] rather than written
/// out, so a rename of the CLI verb cannot leave this offer shelling a name
/// that no longer resolves.
#[cfg(test)]
fn hangar_daemon_id() -> &'static str {
    crate::fleet::daemons::probe::DaemonKind::HangarDaemon.id()
}

/// What the offer is doing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CtaStatus {
    /// Nothing attempted yet: the pane advertises the key.
    #[default]
    Offered,
    /// A start is out. The pane advertises nothing while it is.
    Starting,
    /// The start returned. `ok` is the exit status, `detail` is what it said.
    ///
    /// Kept even when `ok` is true, because a successful `start` on a home that
    /// already has an owner reports "already running" — which is the sentence
    /// an operator needs when the pane is still asking for a daemon afterwards.
    Reported {
        /// Whether the command exited zero.
        ok: bool,
        /// The command's own closing line, never a paraphrase.
        detail: String,
    },
}

/// The offer's state.
#[derive(Debug, Default)]
pub struct DaemonStartCta {
    status: CtaStatus,
    /// Whether the daemon was down the last time anything looked.
    ///
    /// `None` until the first look. Kept so a REPORT can be retired when the
    /// outage it belongs to ends: this offer is one per process, so without it
    /// the pane reopens on the next outage still showing the tick from the
    /// last one.
    last_seen_down: Option<bool>,
}

impl DaemonStartCta {
    /// What the offer is doing right now.
    #[must_use]
    pub const fn status(&self) -> &CtaStatus {
        &self.status
    }

    /// Fold in the host's report of a finished start.
    ///
    /// Returns `true` when anything changed, so the caller marks the frame
    /// dirty without diffing the pane. A report with no start out (the
    /// Daemons screen's own start of the same daemon) changes nothing.
    pub fn finish(&mut self, outcome: &ActionOutcome) -> bool {
        if self.status != CtaStatus::Starting {
            return false;
        }
        // The LAST non-empty line of everything the command said, which is the
        // same line the Daemons screen badges a row with. The full transcript
        // is in that screen's error view; repeating it inside a chat pane would
        // bury the conversation under a daemon log.
        let detail = if outcome.ok {
            outcome.summary.clone()
        } else {
            outcome
                .detail
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("(the command said nothing)")
                .trim()
                .to_string()
        };
        self.status = CtaStatus::Reported {
            ok: outcome.ok,
            detail,
        };
        true
    }

    /// Fold in what the daemon's liveness now looks like.
    ///
    /// A [`CtaStatus::Reported`] describes ONE start, against the outage that
    /// prompted it. Any change in whether the daemon is there ends that outage,
    /// so the report stops being current and the offer goes back to plain
    /// [`CtaStatus::Offered`]. Without this the pane reopens on the NEXT outage
    /// still claiming `✓ already running (pid 4242)` about a daemon that is
    /// down right now — a stale success is worse than no report at all,
    /// because an operator reads it as this outage's.
    ///
    /// Called from the attention refresh rather than from the pane, so a cycle
    /// that happened while the operator was on another tab is still observed:
    /// the refresh reads the poller's cell every tick, far faster than the
    /// poller can change it.
    ///
    /// A start still IN FLIGHT is left alone. Its own landing is what retires
    /// it, and clearing it here would lose the report the operator pressed for.
    ///
    /// Returns `true` when the offer changed, so the caller marks the frame
    /// dirty.
    pub fn observe_daemon(&mut self, down: bool) -> bool {
        if self.last_seen_down == Some(down) {
            return false;
        }
        self.last_seen_down = Some(down);
        if matches!(self.status, CtaStatus::Reported { .. }) {
            self.status = CtaStatus::Offered;
            return true;
        }
        false
    }

    /// Ask for the hangar daemon's start.
    ///
    /// Returns `true` when the caller should queue it: `false` while one is
    /// already out, so key-repeat cannot spawn a second start into a home the
    /// first one is mid-way through taking. The host reports every start it
    /// runs, including one that could not be spawned, so the pane never stays
    /// on `starting…`.
    pub fn start(&mut self) -> bool {
        if self.status == CtaStatus::Starting {
            return false;
        }
        self.status = CtaStatus::Starting;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::daemon::Action;

    fn outcome(ok: bool, summary: &str, detail: &str) -> ActionOutcome {
        ActionOutcome {
            action: Action::Start,
            ok,
            summary: summary.to_string(),
            detail: detail.to_string(),
        }
    }

    /// An offer with no start out advertises the key, and a report of a start
    /// it did not ask for changes nothing.
    #[test]
    fn a_fresh_offer_is_offered_and_ignores_a_start_it_did_not_ask_for() {
        let mut cta = DaemonStartCta::default();
        assert_eq!(cta.status(), &CtaStatus::Offered);
        assert!(
            !cta.finish(&outcome(true, "started", "cmd: …")),
            "a report nobody here asked for must not dirty the frame"
        );
        assert_eq!(cta.status(), &CtaStatus::Offered);
        assert!(cta.start(), "the first press queues the start");
        assert!(!cta.start(), "a second press while it is out does not");
    }

    /// A start that failed reports the command's OWN closing line. A start that
    /// "worked" still reports what it said, because `already running` is a
    /// success the operator has to read.
    #[test]
    fn a_landed_start_reports_what_the_command_said() {
        let mut cta = DaemonStartCta::default();
        cta.start();
        assert!(cta.finish(&outcome(
            false,
            "start failed",
            "cmd: ainb daemon hangar-daemon start\nexit: exit status: 1\n\nstderr:\nrefusing to \
             self-exec a cargo test binary",
        )));
        assert_eq!(
            cta.status(),
            &CtaStatus::Reported {
                ok: false,
                detail: "refusing to self-exec a cargo test binary".to_string(),
            },
            "a failed start must carry the command's last word, not a paraphrase"
        );

        let mut cta = DaemonStartCta::default();
        cta.start();
        assert!(cta.finish(&outcome(true, "already running (pid 4242)", "cmd: …")));
        assert_eq!(
            cta.status(),
            &CtaStatus::Reported {
                ok: true,
                detail: "already running (pid 4242)".to_string(),
            }
        );
    }

    /// A report belongs to ONE outage. The next one must not open still
    /// showing the last one's tick.
    ///
    /// This offer is one per process, so without retiring the report the pane
    /// reopens on a daemon that is down RIGHT NOW while claiming a success from
    /// a cycle the operator may not even remember.
    #[test]
    fn a_report_does_not_survive_into_the_next_outage() {
        let mut cta = DaemonStartCta::default();
        // The outage that prompted the start, then the start landing.
        assert!(
            !cta.observe_daemon(true),
            "the first look reports no change"
        );
        cta.start();
        cta.finish(&outcome(true, "already running (pid 4242)", "cmd: …"));
        assert!(matches!(cta.status(), CtaStatus::Reported { ok: true, .. }));

        // The daemon comes up, then goes down again: the pane reopens on a
        // clean offer, not on the last outage's tick.
        let cleared = cta.observe_daemon(false);
        cta.observe_daemon(true);
        assert_eq!(
            cta.status(),
            &CtaStatus::Offered,
            "the offer reopened carrying the previous outage's result"
        );
        assert!(cleared, "and retiring it must dirty the frame");
    }

    /// A start still in flight is not retired by a liveness reading. Its own
    /// landing is what resolves it, and clearing it here would lose the report
    /// the operator pressed for.
    #[test]
    fn a_start_in_flight_survives_a_liveness_change() {
        let mut cta = DaemonStartCta::default();
        cta.observe_daemon(true);
        cta.status = CtaStatus::Starting;
        assert!(!cta.observe_daemon(false));
        assert_eq!(cta.status(), &CtaStatus::Starting);
    }

    /// The id this shells is the DaemonKind's, so the offer and the Daemons
    /// screen address one daemon.
    #[test]
    fn the_offer_addresses_the_daemon_the_daemons_screen_does() {
        assert_eq!(hangar_daemon_id(), "hangar-daemon");
        assert!(
            crate::cli::daemon::kind_from_id(hangar_daemon_id()).is_some(),
            "the offer must shell a daemon id `ainb daemon` can resolve"
        );
    }
}
