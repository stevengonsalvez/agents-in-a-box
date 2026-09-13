// ABOUTME: `ainb mcp install --codex --copilot` — point OTHER agent CLIs'
// global MCP configs at the pool shim so codex/copilot sessions share the
// same backend processes as Claude sessions.
//
// Both tools read global config, so this is a one-time wiring per tool.
// A `.bak` of the target is written before the first modification and the
// edits are merge-only: entries for other servers are untouched, and an
// existing entry for a pooled name is replaced with the shim (that IS the
// point of installing).

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use toml_edit::{Array, DocumentMut, Item, Table};

use super::{PooledServer, paths};

pub struct InstallReport {
    pub target: PathBuf,
    pub wired: Vec<String>,
}

/// Merge shim entries into `~/.codex/config.toml` (`[mcp_servers.<name>]`).
pub fn install_codex(servers: &[PooledServer]) -> Result<InstallReport> {
    let home = dirs::home_dir().context("home dir")?;
    let target = home.join(".codex").join("config.toml");
    let ainb_exe = std::env::current_exe()?;

    let mut doc: DocumentMut = match std::fs::read_to_string(&target) {
        Ok(content) => {
            backup(&target)?;
            content.parse().with_context(|| format!("parse {}", target.display()))?
        }
        Err(_) => DocumentMut::new(),
    };
    if doc.get("mcp_servers").is_none() {
        doc["mcp_servers"] = Item::Table(Table::new());
    }

    let mut wired = Vec::new();
    for server in servers {
        let socket = paths::server_socket(&server.name)?;
        let mut t = Table::new();
        t["command"] = toml_edit::value(ainb_exe.display().to_string());
        let mut args = Array::new();
        args.push("mcp");
        args.push("proxy");
        args.push(socket.display().to_string());
        t["args"] = toml_edit::value(toml_edit::Value::Array(args));
        doc["mcp_servers"][&server.name] = Item::Table(t);
        wired.push(server.name.clone());
    }

    // 0600 — codex config can carry env secrets; atomic so a torn write
    // never corrupts another tool's config.
    paths::write_atomic(&target, &doc.to_string(), Some(0o600))?;
    Ok(InstallReport { target, wired })
}

/// Merge shim entries into `~/.copilot/mcp-config.json` (`mcpServers.<name>`).
pub fn install_copilot(servers: &[PooledServer]) -> Result<InstallReport> {
    let home = dirs::home_dir().context("home dir")?;
    let target = home.join(".copilot").join("mcp-config.json");
    let ainb_exe = std::env::current_exe()?;

    let mut root: Value = match std::fs::read_to_string(&target) {
        Ok(content) => {
            backup(&target)?;
            serde_json::from_str(&content).with_context(|| format!("parse {}", target.display()))?
        }
        Err(_) => json!({}),
    };
    if !root.is_object() {
        anyhow::bail!("{} root is not an object", target.display());
    }
    let servers_obj =
        root.as_object_mut().unwrap().entry("mcpServers").or_insert_with(|| json!({}));
    let Some(map) = servers_obj.as_object_mut() else {
        anyhow::bail!("mcpServers is not an object in {}", target.display());
    };

    let mut wired = Vec::new();
    for server in servers {
        let socket = paths::server_socket(&server.name)?;
        map.insert(
            server.name.clone(),
            json!({
                "type": "local",
                "command": ainb_exe.display().to_string(),
                "args": ["mcp", "proxy", socket.display().to_string()],
                "tools": ["*"],
            }),
        );
        wired.push(server.name.clone());
    }

    paths::write_atomic(
        &target,
        &(serde_json::to_string_pretty(&root)? + "\n"),
        Some(0o600),
    )?;
    Ok(InstallReport { target, wired })
}

fn backup(target: &Path) -> Result<()> {
    let bak = target.with_extension(format!(
        "{}bak",
        target
            .extension()
            .map(|e| format!("{}.", e.to_string_lossy()))
            .unwrap_or_default()
    ));
    // Never clobber an existing backup: the first run captured the user's
    // pristine pre-install config; a second run would otherwise copy the
    // already-shimmed file over it, destroying the only good copy.
    if bak.exists() {
        return Ok(());
    }
    std::fs::copy(target, &bak).with_context(|| format!("backup {}", bak.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_never_clobbers_a_pristine_copy() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.toml");
        std::fs::write(&target, "pristine\n").unwrap();

        backup(&target).unwrap();
        let bak = target.with_extension("toml.bak");
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "pristine\n");

        // Simulate a second install run over an already-modified file.
        std::fs::write(&target, "shimmed-garbage\n").unwrap();
        backup(&target).unwrap();

        // The backup must still hold the original, not the shimmed state.
        assert_eq!(
            std::fs::read_to_string(&bak).unwrap(),
            "pristine\n",
            "second backup() must not overwrite the good .bak"
        );
    }
}
