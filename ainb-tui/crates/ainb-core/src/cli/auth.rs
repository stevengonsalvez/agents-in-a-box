// ABOUTME: `ainb auth` command — bootstraps Claude credentials via the
// agents-dev Docker container. Lives in the lib (not main.rs) so the
// `CliCommand` registry can dispatch to it.

use anyhow::Result;

pub async fn run_auth_setup() -> Result<()> {
    println!("🔐 Setting up Claude authentication for agents-in-a-box...");
    println!();

    let home_dir =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    let claude_box_dir = home_dir.join(".agents-in-a-box");
    let auth_dir = claude_box_dir.join("auth");

    std::fs::create_dir_all(&auth_dir)
        .map_err(|e| anyhow::anyhow!("Failed to create auth directory: {}", e))?;

    let credentials_path = auth_dir.join(".credentials.json");
    if credentials_path.exists() {
        println!("✅ Authentication already set up!");
        println!("   Credentials found at: {}", credentials_path.display());
        println!();
        println!("To re-authenticate, delete the credentials file and run this command again:");
        println!("   rm {}", credentials_path.display());
        return Ok(());
    }

    println!("📁 Creating auth directories...");
    println!("   Auth directory: {}", auth_dir.display());

    let docker_version =
        std::process::Command::new("docker").args(["--version"]).output().map_err(|e| {
            anyhow::anyhow!(
                "Docker not found: {}. Please install Docker and try again.",
                e
            )
        })?;

    if !docker_version.status.success() {
        return Err(anyhow::anyhow!(
            "Docker is not running. Please start Docker and try again."
        ));
    }

    println!("🏗️  Building authentication container (agents-dev)...");
    let build_status = std::process::Command::new("docker")
        .args(["build", "-t", "agents-box:agents-dev", "docker/agents-dev"])
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to build container: {}", e))?;

    if !build_status.success() {
        return Err(anyhow::anyhow!(
            "Container build failed. Please check Docker and try again."
        ));
    }

    println!();
    println!("🚀 Running authentication setup...");
    println!("   This will prompt you to enter your Anthropic API token.");
    println!();

    let status = std::process::Command::new("docker")
        .args([
            "run",
            "--rm",
            "-it",
            "-v",
            &format!("{}:/home/claude-user/.claude", auth_dir.display()),
            "-e",
            "PATH=/home/claude-user/.npm-global/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            "-e",
            "HOME=/home/claude-user",
            "-w",
            "/home/claude-user",
            "--user",
            "claude-user",
            "--entrypoint",
            "bash",
            "agents-box:agents-dev",
            "-c",
            "/app/scripts/auth-setup.sh",
        ])
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to run auth container: {}", e))?;

    if status.success() {
        println!();
        println!("🎉 Authentication setup complete!");
        println!("   Credentials saved to: {}", credentials_path.display());
        println!();
        println!("You can now create agents-box development sessions with:");
        println!("   agents-box");
    } else {
        println!();
        println!("❌ Authentication setup failed!");
        println!("   Please check the output above for errors and try again.");
        std::process::exit(1);
    }

    Ok(())
}
