// ABOUTME: Keeps every mirror frame on the one seam. `AppState` and the sections
// cannot be serialised at all, and every other serialisation call site in the
// host crates is a locked, triaged list.
//
// Frame-only redaction (`wire::fields::*_in_frame`) is live only inside
// `wire::serialize_section`. A `serde_json::to_value(&app_config)` anywhere else
// writes the real bot tokens, which is right for `save()` and wrong for a
// transport. The compiler cannot tell the two apart, so the list does: a new call
// site fails here until someone has decided which one it is.
//
// The scan is literal text, and that is its ceiling: it finds the paths in
// `CALL` spelled out on one line. A `use serde_json::to_value;` then a bare
// `to_value(`, a call split across lines, a macro that expands to one, or
// `Serialize::serialize` driven by hand all pass unseen. The compile-time probe
// above is the hard guarantee for `AppState` and the sections; this list is a
// review prompt for everything else.

use ainb_app::AppState;
use ainb_app::app::sections::*;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// `true` when `T: Serialize`, `false` otherwise, decided at compile time by
/// method resolution preferring the by-value impl over the by-reference one.
macro_rules! is_serialize {
    ($ty:ty) => {{
        struct Probe<T>(std::marker::PhantomData<T>);
        // Only one of the two traits is picked for a given `T`, so the other is
        // unused in that expansion.
        #[allow(dead_code)]
        trait Yes {
            fn check(&self) -> bool {
                true
            }
        }
        impl<T: serde::Serialize> Yes for Probe<T> {}
        #[allow(dead_code)]
        trait No {
            fn check(&self) -> bool {
                false
            }
        }
        impl<T> No for &Probe<T> {}
        (&Probe::<$ty>(std::marker::PhantomData)).check()
    }};
}

#[test]
fn neither_app_state_nor_any_section_can_be_serialised() {
    // The probe itself works: a type that is `Serialize` reads as one.
    assert!(is_serialize!(ainb_app::wire::frame::Frame));

    let sections = [
        ("AppState", is_serialize!(AppState)),
        ("SessionsSection", is_serialize!(SessionsSection)),
        ("SessionLabelsSection", is_serialize!(SessionLabelsSection)),
        ("TmuxSection", is_serialize!(TmuxSection)),
        ("SshSection", is_serialize!(SshSection)),
        ("GitViewSection", is_serialize!(GitViewSection)),
        ("WorkspaceLoadSection", is_serialize!(WorkspaceLoadSection)),
        ("NewSessionSection", is_serialize!(NewSessionSection)),
        ("LogsSection", is_serialize!(LogsSection)),
        ("ClaudeChatSection", is_serialize!(ClaudeChatSection)),
        ("FleetSection", is_serialize!(FleetSection)),
        ("HangarSection", is_serialize!(HangarSection)),
        ("McpPoolSection", is_serialize!(McpPoolSection)),
        ("InboxSection", is_serialize!(InboxSection)),
        ("PluginsHostSection", is_serialize!(PluginsHostSection)),
        ("ConfigSection", is_serialize!(ConfigSection)),
        ("SkillsSection", is_serialize!(SkillsSection)),
        ("RecoverySection", is_serialize!(RecoverySection)),
        ("OnboardingSection", is_serialize!(OnboardingSection)),
        ("ShellSection", is_serialize!(ShellSection)),
        ("AgentStatusSection", is_serialize!(AgentStatusSection)),
        ("UsageSection", is_serialize!(UsageSection)),
    ];
    assert_eq!(sections.len(), ainb_app::SectionId::COUNT + 1);
    let serialisable: Vec<_> =
        sections.iter().filter(|(_, yes)| *yes).map(|(name, _)| *name).collect();
    assert!(
        serialisable.is_empty(),
        "these serialise without the wire seam, so a transport could bypass the frame \
         redaction: {serialisable:?}. Frame them through `wire::serialize_section`."
    );
}

/// The committed call sites, one `path | call` per line.
const COMMITTED: &str = include_str!("fixtures/serialize_call_sites.txt");

const CALL: &[&str] = &[
    "serde_json::to_value(",
    "serde_json::to_string(",
    "serde_json::to_string_pretty(",
    "serde_json::to_vec(",
    "serde_json::to_vec_pretty(",
    "serde_json::to_writer(",
    "serde_json::to_writer_pretty(",
    "toml::to_string(",
    "toml::to_string_pretty(",
    "toml::Value::try_from(",
];

/// Source roots whose code can hold an `AppState` or a moved type.
const ROOTS: &[&str] = &["ainb-app/src", "ainb-core/src", "ainb-web/src"];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// The call and its argument text, up to the matching parenthesis on the line.
fn call_text(line: &str, start: usize) -> String {
    let mut depth = 0usize;
    let mut end = line.len();
    for (offset, ch) in line[start..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = start + offset + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    line[start..end].trim().to_string()
}

fn current_call_sites() -> BTreeSet<String> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut sites = BTreeSet::new();
    for root in ROOTS {
        let mut files = Vec::new();
        rust_files(&crates.join(root), &mut files);
        for file in files {
            let rel =
                file.strip_prefix(&crates).unwrap_or(&file).to_string_lossy().replace('\\', "/");
            // The seam itself (only ainb-app's, not any directory named `wire`),
            // and test-only files.
            if rel.starts_with("ainb-app/src/wire/") || rel.ends_with("_tests.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            let lines: Vec<&str> = text.lines().collect();
            // Brace depth left to close in an in-file `#[cfg(test)] mod`, whose
            // lines are skipped; code after the module is scanned again.
            let mut in_test_module: Option<i64> = None;
            for (index, line) in lines.iter().enumerate() {
                if let Some(depth) = in_test_module.as_mut() {
                    *depth += i64::try_from(line.matches('{').count()).unwrap_or(i64::MAX)
                        - i64::try_from(line.matches('}').count()).unwrap_or(i64::MAX);
                    if *depth <= 0 {
                        in_test_module = None;
                    }
                    continue;
                }
                if line.trim_start().starts_with("#[cfg(test)]")
                    && lines.get(index + 1).is_some_and(|next| {
                        next.trim_start().starts_with("mod ") && next.contains('{')
                    })
                {
                    in_test_module = Some(0);
                    continue;
                }
                if line.trim_start().starts_with("//") {
                    continue;
                }
                for call in CALL {
                    let mut from = 0;
                    while let Some(found) = line[from..].find(call) {
                        let start = from + found;
                        sites.insert(format!("{rel} | {}", call_text(line, start)));
                        from = start + call.len();
                    }
                }
            }
        }
    }
    sites
}

#[test]
fn every_serialisation_call_site_outside_the_wire_seam_is_triaged() {
    let current = current_call_sites();
    if std::env::var_os("UPDATE_SERIALIZE_CALL_SITES").is_some() {
        let mut out = String::from(
            "# Serialisation call sites outside ainb-app/src/wire, locked by tests/serialize_guard.rs.\n\
             # Each writes a disk file, a CLI's own output, a plugin or daemon RPC body, or (ainb-web\n\
             # routes.rs) the web client's daemon attention stream; none is an AppState section.\n\
             # fleet/agent_status_reader.rs is its fake_daemon (test and test-support builds only):\n\
             # the JSON-RPC reply frame a fake daemon writes to the reader under test, a\n\
             # serde_json::Value the test builds from json! literals, never a section or moved type.\n\
             # A new line is triaged first: frames go through wire::serialize_section only.\n\
             # Regenerate: UPDATE_SERIALIZE_CALL_SITES=1 cargo test -p ainb-app --test serialize_guard\n",
        );
        for site in &current {
            out.push_str(site);
            out.push('\n');
        }
        std::fs::write(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/serialize_call_sites.txt"),
            out,
        )
        .expect("write call-site fixture");
        return;
    }
    let committed: BTreeSet<String> = COMMITTED
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect();
    let added: Vec<_> = current.difference(&committed).collect();
    let removed: Vec<_> = committed.difference(&current).collect();
    assert!(
        added.is_empty() && removed.is_empty(),
        "serialisation call sites changed outside ainb-app/src/wire.\n\
         A mirror frame must come from wire::serialize_section, where frame-only \
         redaction is live; a direct serialisation of a section or a moved type \
         (AppConfig, Session, RepositoryPreset, LogEntry, DaemonStatus) writes \
         credentials. If each new site writes disk, a CLI's own output or an RPC \
         body, regenerate with UPDATE_SERIALIZE_CALL_SITES=1.\nadded: {added:#?}\nremoved: {removed:#?}"
    );
}
