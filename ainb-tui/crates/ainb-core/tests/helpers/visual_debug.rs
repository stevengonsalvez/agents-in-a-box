use rexpect::session::{spawn_command, PtySession};
use std::process::Command;

/// Spawn the application in visual debug mode (opens separate terminal window)
pub fn spawn_app_visual() -> Result<PtySession, rexpect::error::Error> {
    #[cfg(feature = "visual-debug")]
    {
        // Open in separate terminal window (macOS)
        let current_dir = std::env::current_dir()
            .map_err(|e| rexpect::error::Error::Io(e))?;

        let script = format!(
            r#"
            tell application "Terminal"
                do script "cd {} && ./target/debug/agents-box"
                activate
            end tell
            "#,
            current_dir.display()
        );

        // For Linux, use: xterm -e or gnome-terminal --
        // For WSL, use: cmd.exe /c start

        std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .spawn()
            .map_err(|e| rexpect::error::Error::Io(e))?;

        // Give terminal time to open
        std::thread::sleep(std::time::Duration::from_secs(2));

        println!("🖥️  Visual debug mode: Terminal window opened");
        println!("   Watch the test execute in the new window");
    }

    // Continue with normal PTY spawn
    spawn_app_silent()
}

/// Spawn the application silently (normal headless mode)
pub fn spawn_app_silent() -> Result<PtySession, rexpect::error::Error> {
    let binary_path = if std::path::Path::new("target/debug/agents-box").exists() {
        "target/debug/agents-box"
    } else {
        "cargo"
    };

    let mut cmd = if binary_path == "cargo" {
        let mut c = Command::new("cargo");
        c.arg("run").arg("--quiet");
        c
    } else {
        Command::new(binary_path)
    };

    cmd.env("RUST_LOG", "error");
    cmd.env("NO_COLOR", "1");

    spawn_command(cmd, Some(15000))
}

// Platform-specific terminal launchers
#[cfg(target_os = "macos")]
pub fn open_terminal(command: &str) {
    let script = format!(
        r#"
        tell application "Terminal"
            do script "{}"
            activate
        end tell
        "#,
        command
    );

    let _ = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .spawn();
}

#[cfg(target_os = "linux")]
pub fn open_terminal(command: &str) {
    // Try xterm first, then gnome-terminal, then konsole
    let terminals = [
        ("xterm", vec!["-e", command]),
        ("gnome-terminal", vec!["--", command]),
        ("konsole", vec!["-e", command]),
    ];

    for (terminal, args) in &terminals {
        if let Ok(_) = std::process::Command::new(terminal)
            .args(args)
            .spawn()
        {
            break;
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn open_terminal(_command: &str) {
    eprintln!("Visual debug mode not supported on this platform");
}
