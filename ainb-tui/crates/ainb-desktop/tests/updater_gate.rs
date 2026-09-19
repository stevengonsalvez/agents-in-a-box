//! The updater's Tauri commands stand outside `Shell::dispatch_renderer`, so
//! the gate they pass through is checked at the source: every `update_*`
//! command in `main.rs` opens with `update_gate(<its own id>)`, the channel
//! setter, the rollback and the removal of the previous are not commands at
//! all (native menu only), and the previous version is removed from one
//! place only, never on a connect.

use std::path::Path;

fn main_rs() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs")).unwrap()
}

/// Each `#[tauri::command]` fn named `update_*`, with its body's first
/// statement.
fn update_commands(source: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut lines = source.lines();
    while let Some(line) = lines.next() {
        if line.trim() != "#[tauri::command]" {
            continue;
        }
        let mut signature = String::new();
        for next in lines.by_ref() {
            signature.push_str(next.trim());
            signature.push(' ');
            if next.contains('{') {
                break;
            }
        }
        let Some(name) = signature.split("fn ").nth(1).and_then(|s| s.split('(').next()) else {
            continue;
        };
        if !name.starts_with("update_") {
            continue;
        }
        let first = lines
            .by_ref()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with("//"))
            .unwrap_or("")
            .to_string();
        out.push((name.to_string(), first));
    }
    out
}

#[test]
fn every_updater_command_opens_with_the_gate_for_its_own_id() {
    let commands = update_commands(&main_rs());
    assert_eq!(commands.len(), 3, "{commands:?}");
    for (name, first) in &commands {
        let id = format!(
            "update::{}",
            name.trim_start_matches("update_").to_uppercase()
        );
        assert!(
            first.starts_with(&format!("update_gate({id})?")),
            "`{name}` opens with `{first}`, not the gate for {id}"
        );
    }
}

#[test]
fn the_setter_the_rollback_and_the_removal_are_not_commands_the_window_can_invoke() {
    let source = main_rs();
    for name in [
        "update_set_settings",
        "update_rollback",
        "update_discard_previous",
    ] {
        assert!(
            !source.contains(&format!("fn {name}(")),
            "`{name}` is a command; a page script could call it"
        );
    }
    let handler = source
        .split("generate_handler![")
        .nth(1)
        .and_then(|s| s.split(']').next())
        .expect("generate_handler!");
    for name in ["update_check", "update_apply", "update_settings"] {
        assert!(handler.contains(name), "{name} is not registered");
    }
    for name in [
        "update_set_settings",
        "update_rollback",
        "update_discard_previous",
    ] {
        assert!(!handler.contains(name), "{name} is registered");
    }
}

#[test]
fn the_previous_version_is_removed_by_its_command_only() {
    let source = main_rs();
    let sites: Vec<usize> = source.match_indices("clear_previous(").map(|(i, _)| i).collect();
    assert_eq!(
        sites.len(),
        1,
        "clear_previous is called from {} places; one rollback slot stays until the person removes it",
        sites.len()
    );
    let before = &source[..sites[0]];
    let owner = before.rfind("fn ").map_or("", |i| &before[i..]);
    assert!(
        owner.starts_with("fn run_update_discard_previous"),
        "clear_previous is called from `{}`",
        owner.lines().next().unwrap_or("")
    );
}
