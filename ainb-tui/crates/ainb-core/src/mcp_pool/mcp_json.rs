// ABOUTME: Per-session `.mcp.json` generation. Merge semantics: pooled
// entries WIN on name conflict; every other user entry is preserved
// verbatim. The file lives at the worktree root — the cwd the agent CLI
// runs in, which is where Claude Code reads project-scoped MCP config.

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::Path;

use super::PooledServer;

/// Write/merge `.mcp.json` in `worktree` so each pooled server points at the
/// `ainb mcp proxy <socket>` shim. Returns the list of server names wired.
/// `session` (when set) is announced to the pool by each shim so
/// `ainb mcp status` can show which sessions share a server.
pub fn write_session_mcp_json(
    worktree: &Path,
    pooled: &[PooledServer],
    session: Option<&str>,
) -> Result<Vec<String>> {
    if pooled.is_empty() {
        return Ok(vec![]);
    }
    let ainb_exe = std::env::current_exe().context("current_exe")?;
    let path = worktree.join(".mcp.json");

    let mut root: Value = if path.exists() {
        let content =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("parse existing {}", path.display()))?
    } else {
        json!({})
    };

    if !root.is_object() {
        anyhow::bail!(".mcp.json root is not an object: {}", path.display());
    }
    let servers = root.as_object_mut().unwrap().entry("mcpServers").or_insert_with(|| json!({}));
    let Some(servers) = servers.as_object_mut() else {
        anyhow::bail!("mcpServers is not an object in {}", path.display());
    };

    let mut wired = Vec::new();
    for server in pooled {
        let socket = super::paths::server_socket(&server.name)?;
        servers.insert(server.name.clone(), shim_entry(&ainb_exe, &socket, session));
        wired.push(server.name.clone());
    }

    // Atomic temp-file + rename: this is the user's OWN .mcp.json and has no
    // backup, so a torn write would be unrecoverable loss of their content.
    let pretty = serde_json::to_string_pretty(&root)? + "\n";
    super::paths::write_atomic(&path, &pretty, None)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(wired)
}

fn shim_entry(ainb_exe: &Path, socket: &Path, session: Option<&str>) -> Value {
    let mut args = vec![
        "mcp".to_string(),
        "proxy".to_string(),
        socket.display().to_string(),
    ];
    if let Some(session) = session {
        args.push("--session".to_string());
        args.push(session.to_string());
    }
    json!({
        "type": "stdio",
        "command": ainb_exe.display().to_string(),
        "args": args,
    })
}

/// Extract poolable stdio server definitions from an `mcpServers` JSON map
/// (project `.mcp.json` or Claude user scope in `~/.claude.json`).
/// Skips http/sse/ws entries (nothing to pool) and entries already pointing
/// at the `ainb mcp proxy` shim (our own output — re-importing it would
/// pool the shim itself).
pub fn parse_stdio_servers(path: &Path) -> Vec<PooledServer> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return vec![];
    };
    let Ok(root) = serde_json::from_str::<Value>(&content) else {
        return vec![];
    };
    extract_stdio_servers(root.get("mcpServers"))
}

/// Same extraction from an already-parsed `mcpServers` value.
pub fn extract_stdio_servers(servers: Option<&Value>) -> Vec<PooledServer> {
    let Some(map) = servers.and_then(Value::as_object) else {
        return vec![];
    };
    let mut out = Vec::new();
    for (name, entry) in map {
        // Untrusted boundary: `.mcp.json` keys become socket filenames and
        // config keys. Reject anything that could traverse or break out.
        if !super::valid_server_name(name) {
            tracing::warn!("mcp_pool: ignoring server with unsafe name {name:?}");
            continue;
        }
        let transport = entry.get("type").and_then(Value::as_str).unwrap_or("stdio");
        if transport != "stdio" {
            continue;
        }
        let Some(command) = entry.get("command").and_then(Value::as_str) else {
            continue;
        };
        let args: Vec<String> = entry
            .get("args")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        if is_shim_invocation(&args) {
            continue;
        }
        let env = entry
            .get("env")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        out.push(PooledServer {
            name: name.clone(),
            command: command.to_string(),
            args,
            env,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn is_shim_invocation(args: &[String]) -> bool {
    args.len() >= 2 && args[0] == "mcp" && args[1] == "proxy"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn pooled(name: &str) -> PooledServer {
        PooledServer {
            name: name.into(),
            command: "sh".into(),
            args: vec![],
            env: HashMap::new(),
        }
    }

    #[test]
    fn merge_preserves_user_entries_and_pooled_wins() {
        let dir = tempfile::tempdir().unwrap();
        let existing = serde_json::json!({
            "mcpServers": {
                "context7": {"command": "npx", "args": ["-y", "@upstash/context7-mcp"]},
                "my-private": {"command": "node", "args": ["server.js"]}
            }
        });
        std::fs::write(
            dir.path().join(".mcp.json"),
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        let wired = write_session_mcp_json(dir.path(), &[pooled("context7")], None).unwrap();
        assert_eq!(wired, vec!["context7".to_string()]);

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap())
                .unwrap();
        let servers = written["mcpServers"].as_object().unwrap();

        // Pooled entry replaced with the shim…
        let ctx = &servers["context7"];
        assert_eq!(ctx["args"][0], "mcp");
        assert_eq!(ctx["args"][1], "proxy");
        assert!(ctx["args"][2].as_str().unwrap().ends_with("context7.sock"));

        // …user's other server untouched.
        assert_eq!(servers["my-private"]["command"], "node");
    }

    #[test]
    fn parse_skips_remote_and_shim_entries_keeps_stdio() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "mcpServers": {
                    "ctx": {"command": "npx", "args": ["-y", "@upstash/context7-mcp"], "env": {"K": "v"}},
                    "remote": {"type": "http", "url": "https://example.com/mcp"},
                    "already-shimmed": {"command": "/usr/local/bin/ainb", "args": ["mcp", "proxy", "/x.sock"]},
                    "typed-stdio": {"type": "stdio", "command": "uvx", "args": ["mcp-server-fetch"]}
                }
            })
            .to_string(),
        )
        .unwrap();

        let servers = parse_stdio_servers(&path);
        let names: Vec<&str> = servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["ctx", "typed-stdio"], "{servers:?}");
        assert_eq!(servers[0].env.get("K").unwrap(), "v");
    }

    #[test]
    fn creates_file_when_absent_and_noop_when_no_pooled() {
        let dir = tempfile::tempdir().unwrap();
        assert!(write_session_mcp_json(dir.path(), &[], None).unwrap().is_empty());
        assert!(
            !dir.path().join(".mcp.json").exists(),
            "no servers → no file"
        );

        write_session_mcp_json(dir.path(), &[pooled("ctx")], None).unwrap();
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap())
                .unwrap();
        // The generated entry must be a shim invocation: ainb mcp proxy <sock>.
        let entry = &written["mcpServers"]["ctx"];
        assert_eq!(entry["args"][0], "mcp");
        assert_eq!(entry["args"][1], "proxy");
        assert!(entry["args"][2].as_str().unwrap().ends_with("ctx.sock"));
    }
}
