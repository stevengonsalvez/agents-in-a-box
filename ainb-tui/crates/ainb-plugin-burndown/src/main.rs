//! Burndown plugin binary entry point.
//!
//! Spawned as a subprocess by `ainb-plugin-runtime`. Reads JSON-RPC
//! frames from stdin, writes responses + reverse-call requests to
//! stdout, logs to stderr (drained by the runtime's stderr forwarder).

use ainb_plugin_burndown::plugin::BurndownPlugin;
use ainb_plugin_sdk::Server;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Send tracing output to stderr so it doesn't pollute the
    // JSON-RPC framing on stdout. The runtime forwards stderr lines
    // into its host log.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .compact()
        .init();

    Server::new(BurndownPlugin::default()).run_stdio().await?;
    Ok(())
}
