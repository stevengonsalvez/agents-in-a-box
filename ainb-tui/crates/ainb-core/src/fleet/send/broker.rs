// ABOUTME: HTTP client for the claude-peers broker (loopback :7899).
//
// Endpoints (POST except /health):
//   GET  /health
//   POST /register      {pid, cwd, git_root, tty, summary}  -> {id}
//   POST /heartbeat     {id}
//   POST /set-summary   {id, summary}
//   POST /list-peers    {cwd?, git_root?, exclude_id?}      -> Peer[]
//   POST /send-message  {from_id, to_id, text}              -> {ok, error?}
//   POST /poll-messages {id}                                -> {messages: [...]}
//   POST /unregister    {id}

use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

fn port() -> u16 {
    std::env::var("CLAUDE_PEERS_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(7899)
}

fn base() -> String {
    format!("http://127.0.0.1:{}", port())
}

pub struct BrokerClient {
    http: reqwest::Client,
}

impl BrokerClient {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("reqwest client init");
        Self { http }
    }
}

impl Default for BrokerClient {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Serialize)]
pub struct RegisterReq<'a> {
    pub pid: u32,
    pub cwd: &'a str,
    pub git_root: Option<&'a str>,
    pub tty: Option<&'a str>,
    pub summary: &'a str,
}

#[derive(Debug, Deserialize)]
pub struct RegisterRes {
    pub id: String,
}

#[derive(Debug, Serialize)]
struct SendMessageReq<'a> {
    from_id: &'a str,
    to_id: &'a str,
    text: &'a str,
}

#[derive(Debug, Deserialize)]
pub struct SendMessageRes {
    pub ok: bool,
    pub error: Option<String>,
}

pub async fn broker_health() -> bool {
    let url = format!("{}/health", base());
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .ok();
    let Some(client) = client else { return false };
    client.get(&url).send().await.is_ok_and(|r| r.status().is_success())
}

pub async fn broker_send(from_id: &str, to_id: &str, text: &str) -> Result<SendMessageRes> {
    let url = format!("{}/send-message", base());
    let req = SendMessageReq { from_id, to_id, text };
    let client = BrokerClient::new();
    let res = client
        .http
        .post(&url)
        .json(&req)
        .send()
        .await
        .context("posting /send-message")?
        .error_for_status()
        .context("broker rejected /send-message")?
        .json::<SendMessageRes>()
        .await
        .context("parsing /send-message response")?;
    Ok(res)
}

pub async fn broker_register(req: RegisterReq<'_>) -> Result<RegisterRes> {
    let url = format!("{}/register", base());
    let client = BrokerClient::new();
    Ok(client
        .http
        .post(&url)
        .json(&req)
        .send()
        .await
        .context("posting /register")?
        .error_for_status()
        .context("broker rejected /register")?
        .json::<RegisterRes>()
        .await
        .context("parsing /register response")?)
}

pub async fn broker_heartbeat(id: &str) -> Result<()> {
    let url = format!("{}/heartbeat", base());
    BrokerClient::new()
        .http
        .post(&url)
        .json(&serde_json::json!({ "id": id }))
        .send()
        .await
        .context("posting /heartbeat")?
        .error_for_status()
        .context("broker rejected /heartbeat")?;
    Ok(())
}

pub async fn broker_unregister(id: &str) -> Result<()> {
    let url = format!("{}/unregister", base());
    BrokerClient::new()
        .http
        .post(&url)
        .json(&serde_json::json!({ "id": id }))
        .send()
        .await
        .context("posting /unregister")?;
    Ok(())
}

pub async fn broker_set_summary(id: &str, summary: &str) -> Result<()> {
    let url = format!("{}/set-summary", base());
    BrokerClient::new()
        .http
        .post(&url)
        .json(&serde_json::json!({ "id": id, "summary": summary }))
        .send()
        .await
        .context("posting /set-summary")?
        .error_for_status()
        .context("broker rejected /set-summary")?;
    Ok(())
}
