//! `wirectl`: host-side harness. `keygen` mints scratch host keys, host id and
//! token; `probe` drives the same `client` code the phone runs, against peerd,
//! including the three negative cases that must fail closed.

use std::os::unix::fs::OpenOptionsExt;
use std::time::{Duration, Instant};

use ainb_wire_mobile::client::{connect_inner, generate_device_keypair, ConnectConfig};
use ainb_wire_mobile::wire;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use clap::{Parser, Subcommand};
use rand::distributions::{Alphanumeric, DistString};

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Write host secrets (0600) and the pairing offer the app bundles.
    Keygen {
        #[arg(long)]
        secrets: std::path::PathBuf,
        #[arg(long)]
        pairing: std::path::PathBuf,
        #[arg(long)]
        url: String,
        #[arg(long, default_value = "lan")]
        transport: String,
    },
    Probe {
        #[arg(long)]
        secrets: std::path::PathBuf,
        #[arg(long)]
        url: String,
        #[arg(long, default_value = "lan")]
        transport: String,
        #[arg(long, default_value_t = 5)]
        runs: u32,
        #[arg(long, default_value_t = 0)]
        heartbeat_seconds: u64,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    match Args::parse().cmd {
        Cmd::Keygen { secrets, pairing, url, transport } => {
            let (hpriv, hpub) = wire::generate_keypair();
            let host_id = Alphanumeric.sample_string(&mut rand::thread_rng(), 16);
            let token = format!("mdt_{}", Alphanumeric.sample_string(&mut rand::thread_rng(), 32));
            let write = |p: &std::path::Path, v: serde_json::Value| -> std::io::Result<()> {
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new().create(true).truncate(true).write(true).mode(0o600).open(p)?;
                writeln!(f, "{}", serde_json::to_string_pretty(&v).unwrap())
            };
            write(&secrets, serde_json::json!({ "host_private": B64.encode(&hpriv), "host_public": B64.encode(&hpub), "host_id": host_id, "token": token }))?;
            write(&pairing, serde_json::json!({ "url": url, "host_public": B64.encode(&hpub), "host_id": host_id, "token": token, "transport": transport }))?;
            println!("wrote {} and {}", secrets.display(), pairing.display());
        }
        Cmd::Probe { secrets, url, transport, runs, heartbeat_seconds } => {
            let s: serde_json::Value = serde_json::from_slice(&std::fs::read(secrets)?)?;
            let hpub = B64.decode(s["host_public"].as_str().unwrap())?;
            let host_id = s["host_id"].as_str().unwrap().to_string();
            let token = s["token"].as_str().unwrap().to_string();
            let dev = generate_device_keypair();
            let cfg = |transport: &str, host_id: &str, hpub: &[u8], hb: u64| ConnectConfig {
                url: url.clone(),
                host_public_key: hpub.to_vec(),
                device_private_key: dev.private_key.clone(),
                transport: transport.into(),
                host_id: host_id.into(),
                heartbeat_interval_ms: hb,
            };
            for run in 1..=runs {
                let t0 = Instant::now();
                let sess = connect_inner(cfg(&transport, &host_id, &hpub, 0)).await?;
                let hello = std::sync::Arc::clone(&sess).hello(token.clone(), "probe".into(), "wirectl".into()).await?;
                let sub = std::sync::Arc::clone(&sess).fleet_subscribe(0).await?;
                let total = t0.elapsed().as_secs_f64() * 1000.0;
                let st = sess.stats();
                println!(
                    "run {run}: ws {:.2} ms, noise {:.2} ms, hello {:.2} ms (proto {}, {} caps, {}), subscribe {:.2} ms (head {}, {}), connect-to-subscribed {:.2} ms",
                    st.ws_connect_ms, st.noise_handshake_ms, hello.rtt_ms, hello.selected_protocol, hello.capabilities, hello.daemon_version, sub.rtt_ms, sub.head_revision, sub.replay_state, total
                );
                sess.close();
            }
            let (_, wrong_pub) = wire::generate_keypair();
            let other_transport = if transport == "lan" { "tailnet" } else { "lan" };
            for (name, c) in [
                ("wrong carrier", cfg(other_transport, &host_id, &hpub, 0)),
                ("unpinned host key", cfg(&transport, &host_id, &wrong_pub, 0)),
                ("wrong host id", cfg(&transport, "AAAAAAAAAAAAAAAA", &hpub, 0)),
            ] {
                match connect_inner(c).await {
                    Ok(_) => println!("NEGATIVE CASE {name}: UNEXPECTEDLY CONNECTED"),
                    Err(e) => println!("negative {name}: {e}"),
                }
            }
            let sess = connect_inner(cfg(&transport, &host_id, &hpub, 0)).await?;
            match std::sync::Arc::clone(&sess).hello("mdt_wrong".into(), "probe".into(), "wirectl".into()).await {
                Ok(_) => println!("NEGATIVE CASE wrong token: UNEXPECTEDLY ACCEPTED"),
                Err(e) => println!("negative wrong token: {e}"),
            }
            if heartbeat_seconds > 0 {
                let sess = connect_inner(cfg(&transport, &host_id, &hpub, 1000)).await?;
                tokio::time::sleep(Duration::from_secs(heartbeat_seconds)).await;
                let st = sess.stats();
                println!("heartbeat: sent {}, pongs {}, last rtt {:.3} ms", st.heartbeats_sent, st.pongs_received, st.last_pong_rtt_ms);
            }
        }
    }
    Ok(())
}
