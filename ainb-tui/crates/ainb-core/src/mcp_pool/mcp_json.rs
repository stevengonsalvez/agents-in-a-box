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
pub fn write_session_mcp_json(worktree: &Path, pooled: &[PooledServer]) -> Result<Vec<String>> {
    if pooled.is_empty() {
        return Ok(vec![]);
    }
    let ainb_exe = std::env::current_exe().context("current_exe")?;
    let path = worktree.join(".mcp.json");

    let mut root: Value = if path.exists() {
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("parse existing {}", path.display()))?
    } else {
        json!({})
    };

    if !root.is_object() {
        anyhow::bail!(".mcp.json root is not an object: {}", path.display());
    }
    let servers = root
        .as_object_mut()
        .unwrap()
        .entry("mcpServers")
        .or_insert_with(|| json!({}));
    let Some(servers) = servers.as_object_mut() else {
        anyhow::bail!("mcpServers is not an object in {}", path.display());
    };

    let mut wired = Vec::new();
    for server in pooled {
        let socket = super::paths::server_socket(&server.name)?;
        servers.insert(server.name.clone(), shim_entry(&ainb_exe, &socket));
        wired.push(server.name.clone());
    }

    let pretty = serde_json::to_string_pretty(&root)?;
    std::fs::write(&path, pretty + "\n").with_context(|| format!("write {}", path.display()))?;
    Ok(wired)
}

fn shim_entry(ainb_exe: &Path, socket: &Path) -> Value {
    json!({
        "type": "stdio",
        "command": ainb_exe.display().to_string(),
        "args": ["mcp", "proxy", socket.display().to_string()],
    })
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

        let wired = write_session_mcp_json(dir.path(), &[pooled("context7")]).unwrap();
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
    fn creates_file_when_absent_and_noop_when_no_pooled() {
        let dir = tempfile::tempdir().unwrap();
        assert!(write_session_mcp_json(dir.path(), &[]).unwrap().is_empty());
        assert!(!dir.path().join(".mcp.json").exists(), "no servers → no file");

        write_session_mcp_json(dir.path(), &[pooled("ctx")]).unwrap();
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap())
                .unwrap();
        assert!(written["mcpServers"]["ctx"]["command"].as_str().unwrap().contains("ainb") || written["mcpServers"]["ctx"]["command"].is_string());
    }
}
