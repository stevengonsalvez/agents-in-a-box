//! `bd` command construction (P2.2).
//!
//! Every `bd` invocation funnels through [`base_cmd`], which pins the resolved
//! binary, sets `BEADS_DIR` explicitly (never relying on `pwd` — see the module
//! docs), and appends `--json`. The per-verb `build_*` helpers then layer the
//! verb and its flags on top so the wire shape lives in exactly one place and is
//! unit-testable without spawning a process.

use std::path::Path;
use std::process::Command;

use super::{BdCreateArgs, BdId, BdListFilter};

/// Base command: `bd --json` with `BEADS_DIR` set to `beads_dir`.
///
/// `--json` is appended last by each verb builder is *not* assumed — it is set
/// here so a builder cannot forget it. Verb + verb-specific flags are inserted
/// before `--json` by the `build_*` helpers via [`Command::arg`] ordering
/// (clap accepts the global `--json` in any position).
pub(crate) fn base_cmd(bin: &Path, beads_dir: &Path) -> Command {
    let mut cmd = Command::new(bin);
    cmd.env("BEADS_DIR", beads_dir).arg("--json");
    cmd
}

/// `bd create <title> [-d <desc>] [--labels <l,l,…>] [--assignee <a>]`.
///
/// Consumes `args` — every field is moved into the constructed command.
///
/// Unlike `bd list` (which takes a singular `--label` flag *repeated* per
/// label for an AND filter), `bd create` accepts `-l, --labels` exactly once
/// as a comma-separated list — so the labels are joined with `,` and emitted
/// as a single `--labels` flag. Verified against `bd 0.49.0`.
pub(crate) fn build_create(mut cmd: Command, args: BdCreateArgs) -> Command {
    cmd.arg("create").arg(args.title);
    if let Some(desc) = args.description {
        cmd.arg("-d").arg(desc);
    }
    if !args.labels.is_empty() {
        cmd.arg("--labels").arg(args.labels.join(","));
    }
    if let Some(assignee) = args.assignee {
        cmd.arg("--assignee").arg(assignee);
    }
    cmd
}

/// `bd close <id> [--reason <r>]`.
pub(crate) fn build_close(mut cmd: Command, id: &BdId, reason: Option<&str>) -> Command {
    cmd.arg("close").arg(id.as_str());
    if let Some(reason) = reason {
        cmd.arg("--reason").arg(reason);
    }
    cmd
}

/// `bd list [-l <label>...] [--since <iso8601>] [--all]`.
///
/// Consumes `filter` — its labels are moved into the constructed command.
///
/// `--all` is emitted when [`BdListFilter::include_closed`] is set: `bd list`
/// hides closed issues by default, so the inbound/reconcile sync paths (which
/// detect a `bd`-side close) must opt in or the closure is invisible.
pub(crate) fn build_list(mut cmd: Command, filter: BdListFilter) -> Command {
    cmd.arg("list");
    for label in filter.labels {
        cmd.arg("-l").arg(label);
    }
    if let Some(since) = filter.since {
        cmd.arg("--since").arg(since.to_rfc3339());
    }
    if filter.include_closed {
        cmd.arg("--all");
    }
    cmd
}

/// `bd show <id>`.
pub(crate) fn build_show(mut cmd: Command, id: &BdId) -> Command {
    cmd.arg("show").arg(id.as_str());
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::ffi::OsStr;
    use std::path::PathBuf;

    /// Collect a command's args (everything after the program) as owned strings.
    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// A base command pinned to a dummy bin + beads dir, mirroring [`base_cmd`].
    fn base() -> Command {
        base_cmd(&PathBuf::from("/usr/bin/bd"), &PathBuf::from("/tmp/beads"))
    }

    #[test]
    fn base_cmd_sets_json_flag_and_beads_dir_env() {
        let cmd = base();
        assert_eq!(args_of(&cmd), vec!["--json".to_string()]);
        let beads_env = cmd
            .get_envs()
            .find(|(k, _)| *k == OsStr::new("BEADS_DIR"))
            .and_then(|(_, v)| v)
            .map(|v| v.to_string_lossy().into_owned());
        assert_eq!(beads_env.as_deref(), Some("/tmp/beads"));
    }

    #[test]
    fn build_create_emits_verb_desc_labels_and_assignee() {
        let cmd = build_create(
            base(),
            BdCreateArgs {
                title: "ship it".into(),
                description: Some("d".into()),
                labels: vec!["foo".into(), "bar".into()],
                assignee: Some("stevie".into()),
            },
        );
        // `--json` (from base) + verb + title + flags, in emission order.
        assert_eq!(
            args_of(&cmd),
            vec![
                "--json", "create", "ship it", "-d", "d", "--labels", "foo,bar", "--assignee",
                "stevie",
            ]
        );
    }

    #[test]
    fn build_create_omits_optional_flags_when_absent() {
        let cmd = build_create(
            base(),
            BdCreateArgs {
                title: "bare".into(),
                description: None,
                labels: vec![],
                assignee: None,
            },
        );
        assert_eq!(args_of(&cmd), vec!["--json", "create", "bare"]);
    }

    #[test]
    fn build_close_emits_verb_id_and_reason() {
        let cmd = build_close(base(), &BdId::from("abc-123"), Some("done"));
        assert_eq!(
            args_of(&cmd),
            vec!["--json", "close", "abc-123", "--reason", "done"]
        );
    }

    #[test]
    fn build_close_omits_reason_when_absent() {
        let cmd = build_close(base(), &BdId::from("abc-123"), None);
        assert_eq!(args_of(&cmd), vec!["--json", "close", "abc-123"]);
    }

    #[test]
    fn build_list_emits_repeated_label_and_since() {
        let since = chrono::Utc.with_ymd_and_hms(2026, 5, 29, 12, 0, 0).unwrap();
        let cmd = build_list(
            base(),
            BdListFilter {
                labels: vec!["hangar-v1".into(), "foo".into()],
                since: Some(since),
                include_closed: false,
            },
        );
        assert_eq!(
            args_of(&cmd),
            vec![
                "--json",
                "list",
                "-l",
                "hangar-v1",
                "-l",
                "foo",
                "--since",
                &since.to_rfc3339(),
            ]
        );
    }

    #[test]
    fn build_list_omits_filters_when_empty() {
        let cmd = build_list(base(), BdListFilter::default());
        assert_eq!(args_of(&cmd), vec!["--json", "list"]);
    }

    #[test]
    fn build_list_emits_all_when_include_closed() {
        // `bd list` hides closed issues by default; `--all` opts them back in so
        // the inbound/reconcile sync can observe a `bd`-side close.
        let cmd = build_list(
            base(),
            BdListFilter {
                labels: vec!["hangar-v1".into()],
                since: None,
                include_closed: true,
            },
        );
        assert_eq!(
            args_of(&cmd),
            vec!["--json", "list", "-l", "hangar-v1", "--all"]
        );
    }

    #[test]
    fn build_show_emits_verb_and_id() {
        let cmd = build_show(base(), &BdId::from("abc-123"));
        assert_eq!(args_of(&cmd), vec!["--json", "show", "abc-123"]);
    }
}
