//! Conformance axes run against the REAL in-tree plugin binaries.
//!
//! `tests/axes.rs` drives a synthetic canary per axis; that proves the
//! RUNTIME is conformant and says nothing about the plugins we ship. This
//! suite runs the axes that are obligations of EVERY ABI-v2 plugin against
//! the actual binaries (the same artifacts a user installs) and prints a
//! plugin x axis matrix.
//!
//! One command:
//!
//! ```text
//! cargo test -p ainb-plugin-cts-v2 --test real_plugin_axes -- --nocapture
//! ```
//!
//! Axis selection rule: an axis belongs here only if it is a protocol
//! obligation of any conformant plugin AND is observable from outside the
//! plugin (a wire reply, a buffer's dimensions, a process exit status, a
//! lifecycle state). Axes that probe a canary's scripted behaviour
//! (`a04_capability_denied` expects the plugin to answer a CLI probe with
//! `-32001`; `a06`/`a07` expect a specific snapshot payload) are not
//! obligations of a real plugin and stay in `tests/axes.rs`.
//!
//! No `#[ignore]`, no silent skips: every cell is `PASS`, `n/a` with a reason
//! derived from the plugin's own manifest, or a named entry in [`WAIVERS`].
//!
//! ## Disk / process safety
//!
//! `$AINB_HANGAR_HOME` is redirected under `CARGO_TARGET_TMPDIR` before any
//! plugin spawns, so the hangar plugin dials a scratch socket path instead of
//! the user's live `~/.agents-in-a-box/hangar.sock`.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use ainb_plugin_cts_v2::real_plugin::{
    IN_TREE_PLUGINS, PluginUnderTest, RawPlugin, declared_capability_keys, resolve_binary,
};
use ainb_plugin_protocol::manifest::Manifest;
use ainb_plugin_runtime::registry::RegisteredPlugin;
use ainb_plugin_runtime::types::{
    CliOutcome, LifecycleState, PluginId, RenderOutcome, RuntimeConfig,
};
use ainb_plugin_runtime::{Runtime, RuntimeHandle, Viewport};

// =====================================================================
// Axis catalogue
// =====================================================================

/// Result of one plugin x axis cell.
#[derive(Debug, Clone)]
enum Outcome {
    Pass,
    Fail(String),
    /// The axis does not apply, with the manifest fact that says so.
    NotApplicable(String),
}

/// A named, reviewable exemption for one cell.
///
/// Empty is the goal. An entry here is a claim that the failure is
/// acceptable for that plugin, and a reviewer has to be able to judge it from
/// the reason alone.
struct Waiver {
    plugin: &'static str,
    axis: &'static str,
    reason: &'static str,
}

const WAIVERS: &[Waiver] = &[Waiver {
    plugin: "hangar-tui",
    axis: "A14.cli_dispatch",
    // Found by this suite's first run. hangar's manifest advertises
    // `cli_namespaces = ["hangar"]`, but `Plugin::cli_dispatch` in
    // ainb-plugin-hangar/src/plugin.rs is an unconditional
    // `RpcError::not_implemented` for every namespace and every argv, so the
    // declaration is an over-claim. It is inert rather than user-visible:
    // `ainb hangar ...` is served natively by the clap subtree in
    // ainb-core/src/cli/hangar/, which never reaches the plugin. Resolving it
    // means either dropping the line from the manifest or implementing the
    // plugin-side dispatch, and both are product calls for the hangar owner,
    // not a change to make inside a test-hardening PR.
    reason: "manifest declares cli_namespaces = [\"hangar\"] but cli_dispatch is an \
             unconditional -32005 stub; `ainb hangar` is served natively by ainb-core's \
             clap subtree, so the declaration is a stale over-claim, not a user-visible break",
}];

/// One matrix column: its label and the function that measures it.
type Axis = (&'static str, fn(&Subject) -> Outcome);

/// Every axis, in matrix-column order.
const AXES: &[Axis] = &[
    ("A01.manifest", axis_manifest_round_trip),
    ("A01.identity", axis_init_identity),
    ("A02.framing", axis_render_framing),
    ("A02.viewport", axis_render_viewport),
    ("A03.unknown_method", axis_unknown_method),
    ("A05.determinism", axis_render_determinism),
    ("A11.shutdown", axis_graceful_shutdown),
    ("A12.crash_recovery", axis_crash_recovery),
    ("A13.quarantine", axis_quarantine),
    ("A14.cli_dispatch", axis_cli_dispatch),
];

/// One plugin, resolved and ready to drive.
struct Subject {
    plugin: PluginUnderTest,
    binary: PathBuf,
    manifest: Manifest,
    manifest_src: String,
}

// =====================================================================
// The matrix
// =====================================================================

#[test]
fn in_tree_plugins_satisfy_the_protocol_they_claim() {
    isolate_hangar_home();

    let subjects: Vec<Subject> = IN_TREE_PLUGINS.iter().map(resolve_subject).collect();

    let mut rows: Vec<(&str, Vec<Outcome>)> = Vec::new();
    for subject in &subjects {
        let cells = AXES
            .iter()
            .map(|(name, run)| {
                let outcome = run(subject);
                eprintln!(
                    "  {:<16} {:<20} {}",
                    subject.plugin.name,
                    name,
                    short(&outcome)
                );
                outcome
            })
            .collect();
        rows.push((subject.plugin.name, cells));
    }

    eprintln!("\n{}", render_matrix(&rows));

    let mut unwaived: Vec<String> = Vec::new();
    let mut stale_waivers: Vec<String> = Vec::new();
    for (plugin, cells) in &rows {
        for ((axis, _), outcome) in AXES.iter().zip(cells) {
            let waived = WAIVERS.iter().find(|w| w.plugin == *plugin && w.axis == *axis);
            match (outcome, waived) {
                (Outcome::Fail(why), None) => {
                    unwaived.push(format!("{plugin} / {axis}: {why}"));
                }
                (Outcome::Pass | Outcome::NotApplicable(_), Some(w)) => {
                    stale_waivers.push(format!(
                        "{plugin} / {axis} is waived (\"{}\") but no longer fails; delete the waiver",
                        w.reason
                    ));
                }
                _ => {}
            }
        }
    }

    assert!(
        unwaived.is_empty() && stale_waivers.is_empty(),
        "real-plugin conformance failed\n\nunwaived failures:\n{}\n\nstale waivers:\n{}",
        if unwaived.is_empty() {
            "  (none)".to_owned()
        } else {
            unwaived.join("\n")
        },
        if stale_waivers.is_empty() {
            "  (none)".to_owned()
        } else {
            stale_waivers.join("\n")
        }
    );
}

/// Point `$AINB_HANGAR_HOME` at a repo-local scratch dir so the hangar
/// plugin's `unix_socket_dial` resolves to a socket that does not exist
/// instead of the user's live daemon.
fn isolate_hangar_home() {
    let home = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("real-plugin-axes-hangar-home");
    std::fs::create_dir_all(&home).expect("create scratch hangar home");
    let live = dirs_home().join(".agents-in-a-box");
    assert!(
        !home.starts_with(&live),
        "scratch hangar home {} must not be inside the live hangar home {}",
        home.display(),
        live.display()
    );
    // The matrix runs as a single test, so nothing races this write.
    std::env::set_var("AINB_HANGAR_HOME", &home);
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/nonexistent"), PathBuf::from)
}

fn resolve_subject(plugin: &PluginUnderTest) -> Subject {
    let binary = resolve_binary(plugin).unwrap_or_else(|e| panic!("{}: {e}", plugin.name));
    let path = plugin.manifest_path();
    let manifest_src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: read {}: {e}", plugin.name, path.display()));
    let manifest: Manifest = toml::from_str(&manifest_src)
        .unwrap_or_else(|e| panic!("{}: parse {}: {e}", plugin.name, path.display()));
    Subject {
        plugin: *plugin,
        binary,
        manifest,
        manifest_src,
    }
}

// =====================================================================
// Rendering
// =====================================================================

fn short(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Pass => "PASS".to_owned(),
        Outcome::NotApplicable(why) => format!("n/a  ({why})"),
        Outcome::Fail(why) => format!("FAIL  {why}"),
    }
}

const fn cell(outcome: &Outcome, waived: bool) -> &'static str {
    match outcome {
        Outcome::Pass => "PASS",
        Outcome::NotApplicable(_) => "n/a",
        Outcome::Fail(_) if waived => "WAIV",
        Outcome::Fail(_) => "FAIL",
    }
}

fn render_matrix(rows: &[(&str, Vec<Outcome>)]) -> String {
    let label_width = rows.iter().map(|(p, _)| p.len()).max().unwrap_or(6).max(6);
    let widths: Vec<usize> = AXES.iter().map(|(a, _)| a.len().max(4)).collect();

    let mut out = String::from("CTS v2 conformance: in-tree plugin binaries\n\n");
    let _ = write!(out, "{:<label_width$}", "plugin");
    for ((axis, _), w) in AXES.iter().zip(&widths) {
        let _ = write!(out, " | {axis:<w$}");
    }
    out.push('\n');
    out.push_str(&"-".repeat(label_width));
    for w in &widths {
        let _ = write!(out, "-+-{}", "-".repeat(*w));
    }
    out.push('\n');

    for (plugin, cells) in rows {
        let _ = write!(out, "{plugin:<label_width$}");
        for (((axis, _), w), outcome) in AXES.iter().zip(&widths).zip(cells) {
            let waived = WAIVERS.iter().any(|wv| wv.plugin == *plugin && wv.axis == *axis);
            let _ = write!(out, " | {:<w$}", cell(outcome, waived));
        }
        out.push('\n');
    }

    out.push_str(
        "\nlegend: PASS = axis satisfied, n/a = axis does not apply per the plugin's \
                  own manifest,\n        WAIV = failing under a named waiver, FAIL = unwaived \
                  failure\n",
    );

    let mut notes: Vec<String> = Vec::new();
    for (plugin, cells) in rows {
        for ((axis, _), outcome) in AXES.iter().zip(cells) {
            match outcome {
                Outcome::NotApplicable(why) => notes.push(format!("  {plugin} / {axis}: {why}")),
                Outcome::Fail(why) => notes.push(format!("  {plugin} / {axis}: FAILED: {why}")),
                Outcome::Pass => {}
            }
        }
    }
    if !notes.is_empty() {
        out.push_str("\nnotes:\n");
        out.push_str(&notes.join("\n"));
        out.push('\n');
    }
    out
}

// =====================================================================
// Runtime plumbing
// =====================================================================

fn build_runtime() -> (Runtime, RuntimeHandle) {
    let cfg = RuntimeConfig {
        respawn_backoff: [
            Duration::from_millis(50),
            Duration::from_millis(100),
            Duration::from_millis(150),
        ],
        failure_window: Duration::from_secs(60),
        ..RuntimeConfig::default()
    };
    Runtime::with_config(cfg).expect("build runtime")
}

/// Register the real binary with its real manifest, so the runtime enforces
/// exactly the capabilities the plugin ships with.
fn register(rt: &Runtime, subject: &Subject) -> PluginId {
    let plugin = RegisteredPlugin::new(
        subject.manifest.clone(),
        subject.binary.clone(),
        subject.plugin.manifest_path(),
    );
    let id = plugin.id.clone();
    rt.register(plugin);
    id
}

/// First render also pays for spawn + `plugin/init`, hence the generous
/// budget: this axis asserts conformance, not latency.
const RENDER_TIMEOUT: Duration = Duration::from_secs(20);

fn block_render(
    rt: &Runtime,
    handle: &RuntimeHandle,
    id: &PluginId,
    w: u16,
    h: u16,
) -> Result<RenderOutcome, String> {
    let rx = handle.render(id, Viewport::new(w, h), 0);
    rt.tokio_handle().block_on(async {
        match tokio::time::timeout(RENDER_TIMEOUT, rx).await {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(_)) => Err("render channel closed".to_owned()),
            Err(_) => Err(format!("no plugin/render reply within {RENDER_TIMEOUT:?}")),
        }
    })
}

fn wait_running(handle: &RuntimeHandle, id: &PluginId) -> Result<(), String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if matches!(handle.lifecycle_state(id), Some(LifecycleState::Running)) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(format!(
        "never reached Running; state = {:?}",
        handle.lifecycle_state(id)
    ))
}

/// Bring a plugin up and return its runtime, handle and id.
fn spawned(subject: &Subject) -> Result<(Runtime, RuntimeHandle, PluginId), String> {
    let (rt, handle) = build_runtime();
    let id = register(&rt, subject);
    block_render(&rt, &handle, &id, 1, 1)?;
    wait_running(&handle, &id)?;
    Ok((rt, handle, id))
}

fn fail<T>(msg: impl Into<String>) -> Result<T, String> {
    Err(msg.into())
}

/// Adapt a `Result`-returning axis body into an [`Outcome`].
fn outcome(body: impl FnOnce() -> Result<(), String>) -> Outcome {
    body().map_or_else(Outcome::Fail, |()| Outcome::Pass)
}

// =====================================================================
// Axes
// =====================================================================

/// A01: the shipped manifest parses as a v2 `Manifest`, survives a TOML
/// round-trip unchanged, declares `abi_version = 2`, and its `[plugin].name`
/// is the name the host will key it by.
fn axis_manifest_round_trip(subject: &Subject) -> Outcome {
    outcome(|| {
        if subject.manifest.plugin.abi_version != 2 {
            return fail(format!(
                "abi_version is {} not 2",
                subject.manifest.plugin.abi_version
            ));
        }
        if subject.manifest.plugin.name != subject.plugin.name {
            return fail(format!(
                "[plugin].name is {:?}, matrix row says {:?}",
                subject.manifest.plugin.name, subject.plugin.name
            ));
        }
        if subject.manifest.plugin.version.is_empty() {
            return fail("[plugin].version is empty");
        }
        let re_encoded =
            toml::to_string(&subject.manifest).map_err(|e| format!("re-encode manifest: {e}"))?;
        let back: Manifest = toml::from_str(&re_encoded)
            .map_err(|e| format!("re-parse round-tripped manifest: {e}"))?;
        if back != subject.manifest {
            return fail("manifest does not survive a TOML round-trip");
        }
        // Guard against a manifest that parses only because every field it
        // declares was silently ignored.
        if subject.manifest_src.trim().is_empty() {
            return fail("manifest file is empty");
        }
        Ok(())
    })
}

/// A01: `plugin/init` must echo the name and version the manifest declares
/// (`PluginInitResult`'s documented contract), so the host can detect a
/// binary/manifest mismatch at spawn.
fn axis_init_identity(subject: &Subject) -> Outcome {
    outcome(|| {
        let mut raw = RawPlugin::spawn(&subject.binary)?;
        let params = serde_json::json!({
            "manifest_path": subject.plugin.manifest_path().to_string_lossy(),
            "granted_capabilities": declared_capability_keys(&subject.manifest),
            "abi_version": 2,
            "config": serde_json::Value::Null,
        });
        match raw.request("plugin/init", &params, Duration::from_secs(15))? {
            Ok(result) => {
                let name = result.get("name").and_then(serde_json::Value::as_str);
                let version = result.get("version").and_then(serde_json::Value::as_str);
                if name != Some(subject.manifest.plugin.name.as_str()) {
                    return fail(format!(
                        "plugin/init echoed name {name:?}, manifest says {:?}",
                        subject.manifest.plugin.name
                    ));
                }
                if version != Some(subject.manifest.plugin.version.as_str()) {
                    return fail(format!(
                        "plugin/init echoed version {version:?}, manifest says {:?}",
                        subject.manifest.plugin.version
                    ));
                }
                Ok(())
            }
            Err((code, message)) => fail(format!("plugin/init returned {code}: {message}")),
        }
    })
}

/// A02: Content-Length framing. A `plugin/render` at 80x24 must come back as
/// a decodable `RenderResult`, and every cell in the (sparse) `WireBuffer`
/// must land inside the buffer the plugin declared. An out-of-bounds cell is
/// a paint the host would silently drop.
fn axis_render_framing(subject: &Subject) -> Outcome {
    outcome(|| {
        let (rt, handle) = build_runtime();
        let id = register(&rt, subject);
        match block_render(&rt, &handle, &id, 80, 24)? {
            RenderOutcome::Ok(buf) => {
                if let Some((coord, _)) =
                    buf.cells.iter().find(|(c, _)| c.x >= buf.width || c.y >= buf.height)
                {
                    return fail(format!(
                        "cell at ({}, {}) is outside the plugin's own {}x{} buffer",
                        coord.x, coord.y, buf.width, buf.height
                    ));
                }
                Ok(())
            }
            other => fail(format!("render did not return Ok: {other:?}")),
        }
    })
}

/// A02: the render contract's other half. A plugin that advertises a screen
/// must paint the viewport it was handed: the buffer it returns has exactly
/// the requested dimensions, so the host's damage tracking and the plugin's
/// idea of the frame agree. `WireBuffer` is SPARSE, so this asserts the
/// declared dimensions, not a full `width * height` cell count.
fn axis_render_viewport(subject: &Subject) -> Outcome {
    if subject.manifest.provides.screens.is_empty() {
        return Outcome::NotApplicable("manifest declares no [provides].screens".to_owned());
    }
    outcome(|| {
        let (rt, handle) = build_runtime();
        let id = register(&rt, subject);
        match block_render(&rt, &handle, &id, 80, 24)? {
            RenderOutcome::Ok(buf) => {
                if buf.width != 80 || buf.height != 24 {
                    return fail(format!("asked for 80x24, got {}x{}", buf.width, buf.height));
                }
                Ok(())
            }
            other => fail(format!("render did not return Ok: {other:?}")),
        }
    })
}

/// A03: an unknown method must come back as JSON-RPC `-32601`, not a hang,
/// a crash, or a success. Probed on the raw wire before `plugin/init` so
/// nothing else is in flight.
fn axis_unknown_method(subject: &Subject) -> Outcome {
    outcome(|| {
        let mut raw = RawPlugin::spawn(&subject.binary)?;
        match raw.request(
            "cts/definitely_not_a_method",
            &serde_json::json!({}),
            Duration::from_secs(10),
        )? {
            Ok(v) => fail(format!("unknown method succeeded with {v}")),
            Err((code, _)) if code == i64::from(ainb_plugin_protocol::errors::METHOD_NOT_FOUND) => {
                Ok(())
            }
            Err((code, message)) => fail(format!(
                "unknown method returned {code} ({message}), expected {}",
                ainb_plugin_protocol::errors::METHOD_NOT_FOUND
            )),
        }
    })
}

/// A05: once a plugin has SETTLED, repeated renders of an unchanged viewport
/// must be byte-identical.
///
/// The canary version of this axis renders a stateless plugin five times and
/// demands five identical buffers. A real plugin legitimately transitions
/// while it comes up (`checking witr…` becomes the detection result, hangar
/// connects to its daemon), so demanding determinism from frame 0 measures
/// startup, not conformance. This axis instead renders until two consecutive
/// frames agree (the settle point), then requires the next
/// [`STEADY_FRAMES`] to be byte-identical. A plugin that paints a clock or a
/// spinner never settles and fails, which is the finding the axis exists for:
/// the host treats an unchanged buffer as a no-op frame.
const SETTLE_BUDGET: usize = 20;
const STEADY_FRAMES: usize = 3;

fn axis_render_determinism(subject: &Subject) -> Outcome {
    outcome(|| {
        let (rt, handle) = build_runtime();
        let id = register(&rt, subject);
        let frame = |n: usize| -> Result<Vec<u8>, String> {
            match block_render(&rt, &handle, &id, 40, 12)? {
                RenderOutcome::Ok(buf) => {
                    serde_json::to_vec(&buf).map_err(|e| format!("serialize render {n}: {e}"))
                }
                other => Err(format!("render {n} did not return Ok: {other:?}")),
            }
        };

        let mut previous = frame(0)?;
        let mut settled_at = None;
        for n in 1..SETTLE_BUDGET {
            let current = frame(n)?;
            if current == previous {
                settled_at = Some(n);
                break;
            }
            previous = current;
        }
        let Some(settled_at) = settled_at else {
            return fail(format!(
                "no two consecutive renders agreed within {SETTLE_BUDGET} frames; \
                 the plugin never reaches a steady state"
            ));
        };

        for n in 0..STEADY_FRAMES {
            let current = frame(settled_at + 1 + n)?;
            if current != previous {
                return fail(format!(
                    "settled at frame {settled_at}, then frame {} differed byte-for-byte",
                    settled_at + 1 + n
                ));
            }
        }
        Ok(())
    })
}

/// A11: `plugin/shutdown` must be acknowledged, and the process must then
/// exit 0 once stdin closes. The observable side effect is the EXIT STATUS,
/// not "the host did not hang".
fn axis_graceful_shutdown(subject: &Subject) -> Outcome {
    outcome(|| {
        let mut raw = RawPlugin::spawn(&subject.binary)?;
        let init = serde_json::json!({
            "manifest_path": subject.plugin.manifest_path().to_string_lossy(),
            "granted_capabilities": declared_capability_keys(&subject.manifest),
            "abi_version": 2,
            "config": serde_json::Value::Null,
        });
        raw.request("plugin/init", &init, Duration::from_secs(15))?
            .map_err(|(c, m)| format!("plugin/init returned {c}: {m}"))?;

        raw.request(
            "plugin/shutdown",
            &serde_json::json!({}),
            Duration::from_secs(5),
        )?
        .map_err(|(c, m)| format!("plugin/shutdown returned {c}: {m}"))?;

        raw.close_stdin();
        match raw.wait_exit(Duration::from_secs(5))? {
            Some(0) => Ok(()),
            Some(code) => fail(format!("exited {code} after a graceful shutdown")),
            None => fail("terminated by a signal after a graceful shutdown"),
        }
    })
}

/// A12: a killed plugin must be respawned and serve renders again.
fn axis_crash_recovery(subject: &Subject) -> Outcome {
    outcome(|| {
        let (rt, handle, id) = spawned(subject)?;
        handle.inject_kill(&id).map_err(|e| format!("inject_kill: {e}"))?;
        std::thread::sleep(Duration::from_millis(300));

        drop(handle.render(&id, Viewport::new(1, 1), 1));
        std::thread::sleep(Duration::from_millis(500));

        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while std::time::Instant::now() < deadline {
            if matches!(block_render(&rt, &handle, &id, 1, 1)?, RenderOutcome::Ok(_)) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        fail("render never succeeded again after the plugin was killed")
    })
}

/// A13: three crashes inside the failure window must quarantine the plugin,
/// and an explicit reload must clear the quarantine.
fn axis_quarantine(subject: &Subject) -> Outcome {
    outcome(|| {
        let (_rt, handle, id) = spawned(subject)?;
        for _ in 0..3 {
            handle.inject_kill(&id).map_err(|e| format!("inject_kill: {e}"))?;
            std::thread::sleep(Duration::from_millis(300));
            drop(handle.render(&id, Viewport::new(1, 1), 0));
            std::thread::sleep(Duration::from_millis(300));
        }

        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut quarantined = false;
        while std::time::Instant::now() < deadline {
            if matches!(
                handle.lifecycle_state(&id),
                Some(LifecycleState::Quarantined)
            ) {
                quarantined = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if !quarantined {
            return fail(format!(
                "3 crashes did not quarantine; state = {:?}",
                handle.lifecycle_state(&id)
            ));
        }

        handle.reload(&id).map_err(|e| format!("reload: {e}"))?;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if matches!(handle.lifecycle_state(&id), Some(LifecycleState::Idle)) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        fail(format!(
            "reload did not clear quarantine; state = {:?}",
            handle.lifecycle_state(&id)
        ))
    })
}

/// A14: a plugin that advertises a CLI namespace must answer
/// `plugin/cli_dispatch` on it with a well-formed result. The exit code is
/// the plugin's business; producing a decodable `CliDispatchResult` instead
/// of an error or a hang is the protocol obligation.
fn axis_cli_dispatch(subject: &Subject) -> Outcome {
    let Some(namespace) = subject.manifest.provides.cli_namespaces.first().cloned() else {
        return Outcome::NotApplicable("manifest declares no [provides].cli_namespaces".to_owned());
    };
    outcome(|| {
        let (rt, handle, id) = spawned(subject)?;
        let rx = handle.dispatch_cli(&id, &namespace, vec!["--help".to_owned()]);
        let out = rt.tokio_handle().block_on(async {
            match tokio::time::timeout(Duration::from_secs(20), rx).await {
                Ok(Ok(outcome)) => Ok(outcome),
                Ok(Err(_)) => Err("cli channel closed".to_owned()),
                Err(_) => Err("no plugin/cli_dispatch reply within 20s".to_owned()),
            }
        })?;
        match out {
            CliOutcome::Ok(_) => Ok(()),
            other => fail(format!(
                "cli_dispatch on the declared namespace {namespace:?} did not return a \
                 well-formed result: {other:?}"
            )),
        }
    })
}
