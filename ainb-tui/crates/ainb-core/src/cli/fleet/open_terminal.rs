// ABOUTME: `ainb fleet open-terminal <tmux-target>` — attach to a session in a
// real terminal window.
//
// The macOS Fleet app can be deeplinked INTO (`ainbfleet://session/<key>`) but
// had no way to send you OUT to the session, so a read-only interview card named
// a pane you then had to find yourself. The terminal choice lives in Rust
// (`[fleet] terminal` in config.toml) rather than in Swift so there is one
// source of truth and the app stays a one-line `Process` call.

use std::io::Write;

use anyhow::{Context, Result};

/// Map the configured token to a macOS application name.
///
/// Unknown tokens fall back to Terminal.app rather than failing: the point of
/// the button is to land you somewhere, and a typo in config should not make it
/// silently do nothing.
fn terminal_app(token: &str) -> &'static str {
    match token.trim().to_ascii_lowercase().as_str() {
        "warp" => "Warp",
        "iterm" | "iterm2" => "iTerm",
        "ghostty" => "Ghostty",
        _ => "Terminal",
    }
}

pub fn execute(matches: &clap::ArgMatches) -> Result<()> {
    // `open -a` is macOS-only, and this verb is registered on every platform.
    // Say so, rather than failing with a generic "launching the terminal".
    if !cfg!(target_os = "macos") {
        anyhow::bail!("`fleet open-terminal` is macOS-only; attach with `tmux attach -t <target>`");
    }
    let target = matches.get_one::<String>("target").context("a tmux target is required")?;
    // tmux targets are shell-interpolated into the script below, so anything
    // that is not a plausible target is refused rather than escaped: these
    // strings arrive from a UI and a session name is never exotic.
    if target.is_empty()
        || !target
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '@' | '%'))
    {
        anyhow::bail!("refusing to open an implausible tmux target: {target}");
    }
    let config = crate::config::AppConfig::load().unwrap_or_default();
    let app = terminal_app(config.fleet.terminal_token());

    // A `.command` file is the one launch shape every macOS terminal honours:
    // `open -a <App> <file>` opens a window running it. Passing the command as
    // argv differs per terminal, and AppleScript needs automation permission we
    // would rather not require.
    let dir = std::env::temp_dir().join("ainb-open-terminal");
    std::fs::create_dir_all(&dir).context("creating the launcher directory")?;
    let script = dir.join(format!("attach-{}.command", sanitize(target)));
    {
        let mut file = std::fs::File::create(&script).context("writing the launcher")?;
        writeln!(file, "#!/bin/bash")?;
        // `-A` attaches if the session exists and creates it otherwise, which is
        // wrong here: a typo would spawn an empty session that looks like the
        // real one. Plain attach fails loudly instead.
        writeln!(file, "exec tmux attach -t {target}")?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .context("making the launcher executable")?;
    }
    let status = std::process::Command::new("open")
        .arg("-a")
        .arg(app)
        .arg(&script)
        .status()
        .context("launching the terminal")?;
    if !status.success() {
        anyhow::bail!("{app} refused to open (is it installed?)");
    }
    println!("opened {target} in {app}");
    Ok(())
}

fn sanitize(target: &str) -> String {
    target
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}
