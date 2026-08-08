//! `ainb-fleet-tools`: serve the copilot's tool table over MCP stdio.
//!
//! Takes no arguments and no secrets in its environment (see
//! [`ainb_fleet_tools::keyfile`]); the daemon token is read from a `0600` file.
//! stdout is the MCP transport, so every log line goes to stderr.

use ainb_fleet_tools::fleet::FleetTools;
use ainb_fleet_tools::keyfile;
use ainb_fleet_tools::server::FleetToolServer;
use rmcp::ServiceExt as _;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    keyfile::reject_arguments(std::env::args())?;
    let server = FleetToolServer::new(FleetTools::new(keyfile::client_from_env()?));
    let running = server.serve(rmcp::transport::stdio()).await?;
    running.waiting().await?;
    Ok(())
}
