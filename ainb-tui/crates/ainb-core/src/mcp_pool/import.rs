// ABOUTME: `ainb mcp import` — convert existing MCP configs into ainb
// `[mcp_servers.*]` definitions so the pool daemon can spawn them.
//
// Sources: project `.mcp.json` (cwd) + Claude user scope (`~/.claude.json`
// top-level `mcpServers`). Target: `.ainb/config.toml` (project, default)
// or the user config (`--user`). Existing entries are never overwritten;
// toml_edit preserves the file's comments and formatting.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, value};

use super::PooledServer;
use super::mcp_json::{extract_stdio_servers, parse_stdio_servers};

pub struct ImportReport {
    pub target: PathBuf,
    pub imported: Vec<String>,
    pub skipped_existing: Vec<String>,
    pub skipped_unresolvable: Vec<String>,
}

pub fn execute(to_user_config: bool) -> Result<ImportReport> {
    let mut sources: Vec<PooledServer> = Vec::new();

    // Project .mcp.json first — its definitions win over user scope.
    let cwd = std::env::current_dir()?;
    sources.extend(parse_stdio_servers(&cwd.join(".mcp.json")));

    // Claude user scope: ~/.claude.json top-level mcpServers.
    if let Some(home) = dirs::home_dir() {
        let claude_json = home.join(".claude.json");
        if let Ok(content) = std::fs::read_to_string(&claude_json) {
            if let Ok(root) = serde_json::from_str::<serde_json::Value>(&content) {
                for server in extract_stdio_servers(root.get("mcpServers")) {
                    if !sources.iter().any(|s| s.name == server.name) {
                        sources.push(server);
                    }
                }
            }
        }
    }

    let target = if to_user_config {
        crate::config::AppConfig::get_user_config_dir()?.join("config.toml")
    } else {
        cwd.join(".ainb").join("config.toml")
    };
    import_into(&target, &sources)
}

fn import_into(target: &Path, sources: &[PooledServer]) -> Result<ImportReport> {
    let mut doc: DocumentMut = match std::fs::read_to_string(target) {
        Ok(content) => content.parse().with_context(|| format!("parse {}", target.display()))?,
        Err(_) => DocumentMut::new(),
    };

    if doc.get("mcp_servers").is_none() {
        doc["mcp_servers"] = Item::Table(Table::new());
    }

    let mut report = ImportReport {
        target: target.to_path_buf(),
        imported: vec![],
        skipped_existing: vec![],
        skipped_unresolvable: vec![],
    };

    for server in sources {
        let existing = doc["mcp_servers"].as_table().is_some_and(|t| t.contains_key(&server.name));
        if existing {
            report.skipped_existing.push(server.name.clone());
            continue;
        }
        if !server.resolvable_on_host() {
            report.skipped_unresolvable.push(server.name.clone());
            continue;
        }
        doc["mcp_servers"][&server.name] = Item::Table(server_table(server));
        report.imported.push(server.name.clone());
    }

    if !report.imported.is_empty() {
        // Atomic so a torn write can't corrupt ainb's own config. Inherit
        // umask (like AppConfig::save) rather than forcing 0600 — keeps
        // perms consistent with the rest of the config file.
        super::paths::write_atomic(target, &doc.to_string(), None)
            .with_context(|| format!("write {}", target.display()))?;
    }
    Ok(report)
}

fn server_table(server: &PooledServer) -> Table {
    let mut t = Table::new();
    t["name"] = value(server.name.as_str());
    t["description"] = value("imported by ainb mcp import");
    t["enabled_by_default"] = value(true);
    t["shared"] = value(true);

    let mut installation = InlineTable::new();
    installation.insert("type", "PreInstalled".into());
    t["installation"] = value(installation);

    let mut definition = InlineTable::new();
    definition.insert("type", "Command".into());
    definition.insert("command", server.command.as_str().into());
    let mut args = Array::new();
    for a in &server.args {
        args.push(a.as_str());
    }
    definition.insert("args", toml_edit::Value::Array(args));
    if !server.env.is_empty() {
        let mut env = InlineTable::new();
        let mut keys: Vec<_> = server.env.keys().collect();
        keys.sort();
        for k in keys {
            env.insert(k, server.env[k].as_str().into());
        }
        definition.insert("env", toml_edit::Value::InlineTable(env));
    }
    t["definition"] = value(definition);
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn server(name: &str, command: &str) -> PooledServer {
        PooledServer {
            name: name.into(),
            command: command.into(),
            args: vec!["-y".into(), "pkg".into()],
            env: HashMap::from([("K".to_string(), "v".to_string())]),
        }
    }

    #[test]
    fn imports_into_fresh_config_and_round_trips_through_appconfig() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(".ainb").join("config.toml");

        let report = import_into(&target, &[server("ctx", "sh"), server("ghost", "no-such-bin-xyz")]).unwrap();
        assert_eq!(report.imported, vec!["ctx"]);
        assert_eq!(report.skipped_unresolvable, vec!["ghost"]);

        // The written TOML must parse as a real AppConfig with a poolable server.
        let content = std::fs::read_to_string(&target).unwrap();
        let config: crate::config::AppConfig = toml::from_str(&content).unwrap();
        assert!(config.mcp_servers["ctx"].shared);
        let pooled = crate::mcp_pool::pooled_servers(&config);
        assert_eq!(pooled.len(), 1);
        assert_eq!(pooled[0].command, "sh");
        assert_eq!(pooled[0].env["K"], "v");
    }

    #[test]
    fn never_overwrites_existing_entries_and_preserves_file_content() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.toml");
        std::fs::write(
            &target,
            "# my precious comment\n[mcp_pool]\nidle_grace_secs = 60\n\n[mcp_servers.ctx]\nname = \"ctx\"\ndescription = \"mine\"\ninstallation = { type = \"PreInstalled\" }\ndefinition = { type = \"Command\", command = \"bun\", args = [] }\n",
        )
        .unwrap();

        let report = import_into(&target, &[server("ctx", "sh"), server("extra", "sh")]).unwrap();
        assert_eq!(report.skipped_existing, vec!["ctx"]);
        assert_eq!(report.imported, vec!["extra"]);

        let content = std::fs::read_to_string(&target).unwrap();
        assert!(content.contains("# my precious comment"), "comment lost:\n{content}");
        assert!(content.contains("command = \"bun\""), "existing entry clobbered:\n{content}");
        assert!(content.contains("[mcp_servers.extra]"), "new entry missing:\n{content}");
    }
}
