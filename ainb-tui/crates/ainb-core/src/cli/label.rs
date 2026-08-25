// ABOUTME: CLI command for durable session labels.
// Labels are stored separately from mutable Git and tmux metadata.

use anyhow::{Result, anyhow};

use super::LabelArgs;
use crate::cli::util::find_session;
use crate::config::{SessionLabelStore, normalize_session_label};

/// Execute `ainb label SESSION --set TEXT` or `--clear`.
pub fn execute(args: LabelArgs) -> Result<()> {
    let tmux_name = resolve_tmux_name(&args.session)?;
    let label = if args.clear {
        None
    } else {
        Some(
            normalize_session_label(
                args.set.as_deref().ok_or_else(|| anyhow!("use --set TEXT or --clear"))?,
            )
            .map_err(anyhow::Error::msg)?
            .ok_or_else(|| anyhow!("Session label cannot be blank. Use --clear instead."))?,
        )
    };

    let mut labels = SessionLabelStore::load();
    labels.set(tmux_name.clone(), label.clone());
    labels.save()?;

    match label {
        Some(label) => println!("Session label set: {tmux_name} -> {label}"),
        None => println!("Session label cleared: {tmux_name}"),
    }
    Ok(())
}

fn resolve_tmux_name(session: &str) -> Result<String> {
    // SSH sessions do not have managed-session metadata. Their tmux names are
    // already exact durable identifiers and deliberately remain unmapped.
    if session.starts_with("ssh-") {
        return Ok(session.to_string());
    }
    Ok(find_session(session)?.tmux_session_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_label_targets_keep_the_exact_tmux_name() {
        assert_eq!(
            resolve_tmux_name("ssh-prod-22").expect("ssh name"),
            "ssh-prod-22"
        );
    }
}
