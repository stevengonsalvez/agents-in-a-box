//! abtop plugin binary entry point.
//!
//! Spawned as a subprocess by `ainb-plugin-runtime`. Reads JSON-RPC
//! frames from stdin, writes responses + reverse-call requests to
//! stdout, logs to stderr (drained by the runtime's stderr forwarder).

use ainb_plugin_abtop::plugin::AbtopPlugin;
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

    // Detect BEFORE the stdio server reads its first frame: the host does
    // not wait for `plugin/init` to be answered before it sends
    // `plugin/render`, so a plugin that resolves its lifecycle in
    // `on_init` can be asked to paint while it is still Unknown. See
    // `AbtopPlugin::detected`.
    Server::new(AbtopPlugin::detected().await).run_stdio().await?;
    Ok(())
}
