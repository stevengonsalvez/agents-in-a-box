//! `peerd`: the scratch peer on a private port. Keys and token come from a
//! 0600 env file written by `wirectl keygen`, never from argv.

use std::sync::{Arc, Mutex};

use ainb_wire_mobile::peer::{serve, PeerConfig};
use base64::Engine;
use clap::Parser;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    listen: String,
    /// JSON file from `wirectl keygen`: host keys, host id, token.
    #[arg(long)]
    secrets: std::path::PathBuf,
    #[arg(long, default_value = "lan")]
    transport: String,
    #[arg(long)]
    log: Option<std::path::PathBuf>,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow_free::Result {
    let args = Args::parse();
    let s: serde_json::Value = serde_json::from_slice(&std::fs::read(&args.secrets)?)?;
    let field = |k: &str| s[k].as_str().map(str::to_owned).ok_or(format!("secrets missing {k}"));
    let cfg = PeerConfig {
        host_private_key: base64::engine::general_purpose::STANDARD.decode(field("host_private")?)?,
        transport: args.transport.clone(),
        host_id: field("host_id")?,
        token: field("token")?,
        log: match &args.log {
            Some(p) => Some(Arc::new(Mutex::new(std::fs::OpenOptions::new().create(true).append(true).open(p)?))),
            None => None,
        },
    };
    let listener = tokio::net::TcpListener::bind(&args.listen).await?;
    eprintln!("peerd listening on {} transport={}", listener.local_addr()?, args.transport);
    serve(listener, Arc::new(cfg)).await;
    Ok(())
}

mod anyhow_free {
    pub type Result = std::result::Result<(), Box<dyn std::error::Error>>;
}
