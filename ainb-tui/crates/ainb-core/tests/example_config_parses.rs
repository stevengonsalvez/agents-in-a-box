// ABOUTME: Proves config/example.config.toml still describes the real schema.

//! `config/example.config.toml` is the only reference most users read, and
//! nothing used to parse it. It drifted badly: the `container_templates`
//! example used an `image_source` tag (`"prebuilt"`) that has never existed,
//! called `working_dir` `working_directory`, gave `memory_limit` a Docker-style
//! `"4g"` string where the field is an integer count of megabytes, and showed
//! `mcp_servers` entries with flat `command` / `args` / `init_strategy` keys
//! that `McpServerConfig` does not have at all. Every one of those blocks would
//! have failed to load.
//!
//! The illustrative blocks have to stay commented out — users copy this file
//! straight to `~/.agents-in-a-box/config/config.toml`, so a live
//! `[container_templates.my-node]` would conjure a template nobody asked for.
//! So each one is fenced with `# --8<-- parse` / `# -->8--`; this test
//! uncomments what is inside the fences and deserializes it as the real
//! `AppConfig`.
//!
//! Adding an example? Put it inside a fence. Changing the schema? This test
//! tells you which block in the docs you just invalidated.

use ainb::config::AppConfig;

const EXAMPLE: &str = include_str!("../../../config/example.config.toml");

const OPEN: &str = "--8<-- parse";
const CLOSE: &str = "-->8--";

/// Strip one leading `# ` (or a bare `#`) from a commented example line.
fn uncomment(line: &str) -> String {
    let trimmed = line.trim_start();
    match trimmed.strip_prefix('#') {
        Some(rest) => rest.strip_prefix(' ').unwrap_or(rest).to_string(),
        None => line.to_string(),
    }
}

/// Every fenced block, uncommented, in file order.
fn fenced_blocks(source: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<Vec<String>> = None;

    for line in source.lines() {
        if line.contains(OPEN) {
            assert!(
                current.is_none(),
                "nested `{OPEN}` fence in example.config.toml"
            );
            current = Some(Vec::new());
            continue;
        }
        if line.contains(CLOSE) {
            let block = current.take().unwrap_or_else(|| {
                panic!("`{CLOSE}` without a matching `{OPEN}` in example.config.toml")
            });
            blocks.push(block.join("\n"));
            continue;
        }
        if let Some(buffer) = current.as_mut() {
            buffer.push(uncomment(line));
        }
    }

    assert!(
        current.is_none(),
        "unclosed `{OPEN}` fence in example.config.toml"
    );
    blocks
}

/// The uncommented part of the file — everything a user gets by copying it
/// verbatim — must load as-is.
#[test]
fn live_portion_of_the_example_config_loads() {
    let live: String = EXAMPLE
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");

    toml::from_str::<AppConfig>(&live)
        .expect("the uncommented part of example.config.toml does not deserialize as AppConfig");
}

/// Each fenced example block must deserialize too, so a documented example can
/// never describe a shape the code will reject.
#[test]
fn every_fenced_example_block_loads() {
    let blocks = fenced_blocks(EXAMPLE);
    assert!(
        !blocks.is_empty(),
        "no `{OPEN}` fences found — did the sentinels get renamed?"
    );

    for (i, block) in blocks.iter().enumerate() {
        if let Err(err) = toml::from_str::<AppConfig>(block) {
            panic!(
                "fenced example block {} in config/example.config.toml does not \
                 deserialize as AppConfig: {err}\n--- block ---\n{block}",
                i + 1
            );
        }
    }
}

/// The whole file, with every fenced block enabled, must ALSO load — this is
/// what a user gets if they uncomment the examples, and it catches two blocks
/// that parse alone but collide (a duplicate key, say).
#[test]
fn the_example_config_loads_with_all_examples_enabled() {
    let mut combined = EXAMPLE
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    for block in fenced_blocks(EXAMPLE) {
        combined.push('\n');
        combined.push_str(&block);
    }

    toml::from_str::<AppConfig>(&combined)
        .expect("example.config.toml does not load with its example blocks enabled");
}

#[test]
fn uncomment_strips_exactly_one_hash_and_space() {
    assert_eq!(uncomment("# key = 1"), "key = 1");
    assert_eq!(uncomment("#key = 1"), "key = 1");
    assert_eq!(uncomment("#  indented = 1"), " indented = 1");
    assert_eq!(uncomment(""), "");
    // A trailing comment on a real line survives, so `memory_limit = 4096   #
    // megabytes` keeps its note.
    assert_eq!(uncomment("# a = 1   # note"), "a = 1   # note");
}
