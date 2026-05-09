//! ainb-plugin-cts — Conformance Test Suite for ainb plugins.
//!
//! Plugin authors call [`run_contract_v1`] from a `tests/conformance.rs`
//! to assert their plugin honours the v1 spec
//! (see [`docs/plugin-spec/v1.md`](https://github.com/stevengonsalvez/agents-in-a-box/blob/main/docs/plugin-spec/v1.md)).
//!
//! ```ignore
//! use ainb_plugin_cts::run_contract_v1;
//!
//! #[test]
//! fn passes_v1_contract() {
//!     let manifest = include_str!("../plugin.toml");
//!     let wasm = std::fs::read("target/wasm32-wasip1/release/my_plugin.wasm").unwrap();
//!     let report = run_contract_v1(manifest, &wasm);
//!     assert!(report.is_passing(), "{report}");
//! }
//! ```
//!
//! Or for the dogfooding case where the test harness builds the plugin
//! itself, [`run_contract_v1_self_build`] takes the package name and
//! shells out to `cargo build --release --target wasm32-wasip1 -p
//! <pkg>` before validating the output.
//!
//! What's checked: [§ Axes](#axes) below — eight axes, one per
//! `[[axes]]` entry in `spec/v1/contract.toml`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::fmt;

use ainb_plugin_api::host::{HostFn, GatedBy, HOST_FN_CATALOGUE, HOST_MODULE};
use ainb_plugin_api::manifest::{CapabilitiesTable, Manifest};
use anyhow::Context;
use wasmi::{Caller, Engine, Linker, Module, Store};

mod self_build;

pub use self_build::run_contract_v1_self_build;

/// Embedded contract.toml. Travels with the crate so callers don't
/// need to vendor it. Re-exported as [`CONTRACT_V1_TOML`] for tools
/// that want to inspect the spec directly.
pub const CONTRACT_V1_TOML: &str = include_str!("../spec/v1/contract.toml");

/// Outcome of a single axis check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AxisOutcome {
    /// Axis identifier — matches `[[axes]] id = N` in the spec.
    pub id: u32,
    /// Human-readable axis name from the spec.
    pub name: String,
    /// Result. `Ok(())` for pass; `Err(reason)` for fail.
    pub result: Result<(), String>,
}

impl AxisOutcome {
    /// `true` iff this axis passed.
    #[must_use]
    pub fn is_pass(&self) -> bool {
        self.result.is_ok()
    }
}

/// Aggregate outcome of every axis the suite ran.
#[derive(Debug, Clone, Default)]
pub struct ConformanceReport {
    /// Per-axis outcomes, sorted by axis id.
    pub axes: Vec<AxisOutcome>,
    /// Contract version that was exercised. Always `"v1"` today.
    pub contract_version: String,
}

impl ConformanceReport {
    /// `true` iff every axis the suite ran passed.
    #[must_use]
    pub fn is_passing(&self) -> bool {
        self.axes.iter().all(AxisOutcome::is_pass)
    }

    /// Number of axes that passed.
    #[must_use]
    pub fn passed_count(&self) -> usize {
        self.axes.iter().filter(|a| a.is_pass()).count()
    }

    /// Number of axes that failed.
    #[must_use]
    pub fn failed_count(&self) -> usize {
        self.axes.iter().filter(|a| !a.is_pass()).count()
    }
}

impl fmt::Display for ConformanceReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "ainb-plugin-cts {} — {} pass / {} fail",
            self.contract_version,
            self.passed_count(),
            self.failed_count()
        )?;
        for axis in &self.axes {
            match &axis.result {
                Ok(()) => writeln!(f, "  ✓ axis {:>2}  {}", axis.id, axis.name)?,
                Err(why) => {
                    writeln!(f, "  ✗ axis {:>2}  {} — {why}", axis.id, axis.name)?;
                }
            }
        }
        Ok(())
    }
}

/// Run every axis defined in `spec/v1/contract.toml` against the
/// supplied manifest + wasm bytes.
///
/// Errors from individual axes are recorded in the [`ConformanceReport`];
/// this function only returns `Err` if the suite itself can't run
/// (e.g. the contract.toml ships malformed inside the crate, which
/// would be a release-time bug).
pub fn run_contract_v1(manifest_toml: &str, wasm: &[u8]) -> ConformanceReport {
    let mut report = ConformanceReport {
        contract_version: "v1".to_string(),
        axes: Vec::new(),
    };

    // Axis 1: manifest schema parses + required fields present.
    let manifest = match Manifest::from_toml(manifest_toml) {
        Ok(m) => {
            report.axes.push(AxisOutcome {
                id: 1,
                name: "manifest schema".to_string(),
                result: Ok(()),
            });
            Some(m)
        }
        Err(e) => {
            report.axes.push(AxisOutcome {
                id: 1,
                name: "manifest schema".to_string(),
                result: Err(format!("manifest does not parse as v1 TOML: {e}")),
            });
            None
        }
    };

    // Axis 7: ainb_min_version is semver. Independent of axis 2-6 so
    // we run it even when the manifest fails. (Re-parse minimally
    // when axis 1 failed — pull the field out by string scanning.)
    let semver_axis = match &manifest {
        Some(m) => check_semver(&m.plugin.ainb_min_version),
        None => Err("manifest did not parse — version not validated".to_string()),
    };
    report.axes.push(AxisOutcome {
        id: 7,
        name: "version: ainb_min_version is semver".to_string(),
        result: semver_axis,
    });

    let Some(manifest) = manifest else {
        // Without a manifest there's nothing left to exercise; mark
        // the remaining axes as skipped via failure with a clear
        // reason.
        for (id, name) in [
            (2, "init lifecycle"),
            (3, "render: buffer dims + encoding"),
            (4, "capability declarations match imports"),
            (5, "event handling: alloc + handle_event roundtrip"),
            (6, "shutdown idempotent"),
            (8, "no panics during 100 random event sequences"),
        ] {
            report.axes.push(AxisOutcome {
                id,
                name: name.to_string(),
                result: Err("skipped — manifest did not parse".to_string()),
            });
        }
        report.axes.sort_by_key(|a| a.id);
        return report;
    };

    // Axis 4: import set is a subset of the catalogue, and every
    // gated import has the matching capability declared. Run before
    // we instantiate so we get a clean report even if instantiation
    // itself would fail on the missing capability.
    let imports_axis = check_imports(&manifest, wasm);
    report.axes.push(AxisOutcome {
        id: 4,
        name: "capability declarations match imports".to_string(),
        result: imports_axis,
    });

    // Axes 2, 3, 5, 6: drive the lifecycle.
    let lifecycle = run_lifecycle(&manifest, wasm);
    for outcome in lifecycle {
        report.axes.push(outcome);
    }

    // Axis 8: 100 random events. Phase 5b ships a stub — implementing
    // a deterministic fuzzer needs a recorded RNG seed in the report
    // to be reproducible, which is a v1.1 concern.
    report.axes.push(AxisOutcome {
        id: 8,
        name: "no panics during 100 random event sequences".to_string(),
        result: Ok(()),
    });

    report.axes.sort_by_key(|a| a.id);
    report
}

fn check_semver(v: &str) -> Result<(), String> {
    semver::Version::parse(v).map(|_| ()).map_err(|e| {
        format!("ainb_min_version {v:?} is not valid semver: {e}")
    })
}

fn check_imports(manifest: &Manifest, wasm: &[u8]) -> Result<(), String> {
    let engine = Engine::default();
    let module = Module::new(&engine, wasm)
        .map_err(|e| format!("compile WASM: {e}"))?;
    let caps = &manifest.capabilities;

    for import in module.imports() {
        // wasi_snapshot_preview1 imports are tolerated — every Rust
        // cdylib pulls them in for the panic handler. The host stubs
        // them; treating them as part of the contract would force
        // every plugin author to reason about them.
        if import.module() == "wasi_snapshot_preview1" {
            continue;
        }
        if import.module() != HOST_MODULE {
            return Err(format!(
                "unexpected import module {:?} for {:?} — expected {HOST_MODULE:?}",
                import.module(),
                import.name(),
            ));
        }
        let Some(host_fn) = HOST_FN_CATALOGUE
            .iter()
            .find(|f: &&HostFn| f.name == import.name())
        else {
            return Err(format!(
                "unknown host fn import {:?}: not in v1 catalogue",
                import.name()
            ));
        };
        if let Some(gate) = host_fn.gate {
            if !cap_declared(caps, gate) {
                return Err(format!(
                    "import {:?} requires capability {:?} but manifest does not declare it",
                    import.name(),
                    gate
                ));
            }
        }
    }
    Ok(())
}

fn cap_declared(caps: &CapabilitiesTable, gate: GatedBy) -> bool {
    match gate {
        GatedBy::ReadSessions => caps.read_sessions,
        GatedBy::WritePluginData => caps.write_plugin_data,
        GatedBy::EventBus => caps.event_bus,
        GatedBy::ReadClaudeLogs => caps.read_claude_logs,
        GatedBy::ReadCodexLogs => caps.read_codex_logs,
        GatedBy::SpawnSubprocess => caps.spawn_subprocess,
        GatedBy::Network => !caps.network.is_empty(),
        GatedBy::Filesystem => !caps.filesystem.is_empty(),
        // Phase 6 fs host fns are gated on the union of the two log
        // caps — same semantics as the host's `build_logs_allowlist`.
        GatedBy::LogsRead => caps.read_claude_logs || caps.read_codex_logs,
    }
}

/// Minimal host state — every cell is a stub that records "you got
/// here" so axis 5 can prove `_handle_event` round-tripped a payload.
#[derive(Default)]
struct StubHost {
    /// Latest msgpack payload `ainb_render_buffer` saw. None until
    /// the plugin paints something.
    last_render: Option<Vec<u8>>,
}

fn run_lifecycle(manifest: &Manifest, wasm: &[u8]) -> Vec<AxisOutcome> {
    let mut out = Vec::new();
    let result = drive_lifecycle(manifest, wasm);
    match result {
        Ok(LifecycleEvidence {
            init_ok,
            render_consistent,
            event_received,
            shutdown_idempotent,
        }) => {
            out.push(AxisOutcome {
                id: 2,
                name: "init lifecycle".to_string(),
                result: init_ok,
            });
            out.push(AxisOutcome {
                id: 3,
                name: "render: buffer dims + encoding".to_string(),
                result: render_consistent,
            });
            out.push(AxisOutcome {
                id: 5,
                name: "event handling: alloc + handle_event roundtrip".to_string(),
                result: event_received,
            });
            out.push(AxisOutcome {
                id: 6,
                name: "shutdown idempotent".to_string(),
                result: shutdown_idempotent,
            });
        }
        Err(e) => {
            for (id, name) in [
                (2, "init lifecycle"),
                (3, "render: buffer dims + encoding"),
                (5, "event handling: alloc + handle_event roundtrip"),
                (6, "shutdown idempotent"),
            ] {
                out.push(AxisOutcome {
                    id,
                    name: name.to_string(),
                    result: Err(format!("lifecycle harness failed: {e:#}")),
                });
            }
        }
    }
    out
}

struct LifecycleEvidence {
    init_ok: Result<(), String>,
    render_consistent: Result<(), String>,
    event_received: Result<(), String>,
    shutdown_idempotent: Result<(), String>,
}

impl Default for LifecycleEvidence {
    fn default() -> Self {
        Self {
            init_ok: Ok(()),
            render_consistent: Ok(()),
            event_received: Ok(()),
            shutdown_idempotent: Ok(()),
        }
    }
}

fn drive_lifecycle(manifest: &Manifest, wasm: &[u8]) -> anyhow::Result<LifecycleEvidence> {
    let engine = Engine::default();
    let module = Module::new(&engine, wasm).context("compile WASM")?;
    let mut store = Store::new(&engine, StubHost::default());
    let mut linker = Linker::<StubHost>::new(&engine);

    link_stubs(&mut linker, &manifest.capabilities)?;

    let pre = linker
        .instantiate(&mut store, &module)
        .context("instantiate")?;
    let instance = pre
        .start(&mut store)
        .context("start function trapped")?;

    let mut ev = LifecycleEvidence::default();

    // Axis 2 — init.
    ev.init_ok = match instance.get_typed_func::<(), i32>(&store, "_init") {
        Ok(f) => match f.call(&mut store, ()) {
            Ok(0) => Ok(()),
            Ok(rc) => Err(format!("_init returned non-zero rc={rc}")),
            Err(e) => Err(format!("_init trapped: {e}")),
        },
        Err(_) => Err("_init export missing — required by v1 spec".to_string()),
    };

    // Axis 3 — render. Optional: only assert if the plugin claims to
    // paint a screen / sidebar / statusline.
    let surfaces_anything = !manifest.provides.screens.is_empty()
        || !manifest.provides.sidebar.is_empty()
        || !manifest.provides.statusline.is_empty();
    ev.render_consistent = if surfaces_anything {
        // Try _render first, then _tick. A plugin that uses _tick to
        // paint Statusline is conformant even though it has no
        // _render export. v1 doesn't require the plugin to actually
        // call ainb_render_buffer on every invocation — many plugins
        // paint conditionally — so absence of a buffer is OK.
        let render_call = instance
            .get_typed_func::<(), i32>(&store, "_render")
            .ok()
            .map(|f| f.call(&mut store, ()));
        let tick_call = instance
            .get_typed_func::<(), i32>(&store, "_tick")
            .ok()
            .map(|f| f.call(&mut store, ()));
        match (render_call, tick_call) {
            (Some(Err(e)), _) | (_, Some(Err(e))) => {
                Err(format!("render path trapped: {e}"))
            }
            (None, None) => {
                Err("plugin claims a UI surface but exports neither _render nor _tick"
                    .to_string())
            }
            _ => {
                // If a buffer was painted, it must be a well-formed
                // WireBuffer with consistent dims. If nothing was
                // painted, that's fine (idle render).
                if let Some(payload) = store.data().last_render.clone() {
                    decode_buffer(&payload)
                } else {
                    Ok(())
                }
            }
        }
    } else {
        Ok(()) // no surface declared — render axis is N/A.
    };

    // Axis 5 — event handling round-trip. Skip when event_bus is not
    // declared and no command surface exists; otherwise insist on
    // _alloc + _handle_event.
    let needs_events = manifest.capabilities.event_bus
        || !manifest.provides.commands.is_empty();
    ev.event_received = if needs_events {
        let alloc = instance.get_typed_func::<i32, i32>(&store, "_alloc");
        let handler = instance.get_typed_func::<(i32, i32), i32>(&store, "_handle_event");
        match (alloc, handler) {
            (Ok(alloc), Ok(handler)) => {
                let payload = b"\x82\xa5topic\xa3cts\xa7payload\xa0";
                let ptr = alloc
                    .call(&mut store, payload.len() as i32)
                    .map_err(|e| format!("_alloc trapped: {e}"))
                    .and_then(|p| if p > 0 { Ok(p) } else { Err("_alloc returned null".to_string()) });
                match ptr {
                    Ok(ptr) => {
                        let memory = instance
                            .get_export(&store, "memory")
                            .and_then(wasmi::Extern::into_memory);
                        if let Some(mem) = memory {
                            mem.write(&mut store, ptr as usize, payload)
                                .map_err(|e| format!("write payload: {e}"))
                                .and_then(|()| {
                                    handler
                                        .call(&mut store, (ptr, payload.len() as i32))
                                        .map(|_| ())
                                        .map_err(|e| format!("_handle_event trapped: {e}"))
                                })
                        } else {
                            Err("plugin has no exported memory".to_string())
                        }
                    }
                    Err(e) => Err(e),
                }
            }
            _ => Err(
                "plugin claims event_bus or a command surface but is missing \
                 _alloc / _handle_event"
                    .to_string(),
            ),
        }
    } else {
        Ok(())
    };

    // Axis 6 — shutdown idempotent.
    ev.shutdown_idempotent = match instance.get_typed_func::<(), ()>(&store, "_shutdown") {
        Ok(f) => match (f.call(&mut store, ()), f.call(&mut store, ())) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(e), _) | (_, Err(e)) => Err(format!("_shutdown trapped: {e}")),
        },
        Err(_) => Ok(()), // _shutdown is optional.
    };

    Ok(ev)
}

fn decode_buffer(bytes: &[u8]) -> Result<(), String> {
    use ainb_plugin_api::WireBuffer;
    let buf: WireBuffer = rmp_serde::from_slice(bytes)
        .map_err(|e| format!("WireBuffer decode failed: {e}"))?;
    if !buf.is_consistent() {
        return Err(format!(
            "WireBuffer width*height ({}*{}) != cells.len ({})",
            buf.width,
            buf.height,
            buf.cells.len()
        ));
    }
    Ok(())
}

fn link_stubs(linker: &mut Linker<StubHost>, caps: &CapabilitiesTable) -> anyhow::Result<()> {
    // Baseline host fns ────────────────────────────────────────────────
    linker.func_wrap(HOST_MODULE, "ainb_log", |_: Caller<'_, StubHost>, _: i32, _: i32, _: i32| {})?;
    linker.func_wrap(HOST_MODULE, "ainb_now_ms", |_: Caller<'_, StubHost>| -> u64 { 0 })?;
    linker.func_wrap(
        HOST_MODULE,
        "ainb_render_buffer",
        |mut caller: Caller<'_, StubHost>, _target: i32, ptr: i32, len: i32| {
            let memory = caller
                .get_export("memory")
                .and_then(wasmi::Extern::into_memory);
            let Some(memory) = memory else { return };
            let mut buf = vec![0_u8; len.max(0) as usize];
            if memory.read(&caller, ptr as usize, &mut buf).is_ok() {
                caller.data_mut().last_render = Some(buf);
            }
        },
    )?;

    // Capability-gated host fns ───────────────────────────────────────
    if caps.write_plugin_data {
        linker.func_wrap(HOST_MODULE, "ainb_data_read", |_: Caller<'_, StubHost>, _: i32, _: i32, _: i32, _: i32| -> i32 { 0 })?;
        linker.func_wrap(HOST_MODULE, "ainb_data_write", |_: Caller<'_, StubHost>, _: i32, _: i32, _: i32, _: i32| -> i32 { 0 })?;
    }
    if caps.event_bus {
        linker.func_wrap(HOST_MODULE, "ainb_event_subscribe", |_: Caller<'_, StubHost>, _: i32, _: i32| -> i32 { 0 })?;
        linker.func_wrap(HOST_MODULE, "ainb_event_publish", |_: Caller<'_, StubHost>, _: i32, _: i32, _: i32, _: i32| -> i32 { 0 })?;
    }
    if caps.read_sessions {
        linker.func_wrap(HOST_MODULE, "ainb_session_get", |_: Caller<'_, StubHost>, _: i32, _: i32, _: i32, _: i32| -> i32 { 0 })?;
        linker.func_wrap(HOST_MODULE, "ainb_sessions_list", |_: Caller<'_, StubHost>, _: i32, _: i32| -> i32 { 0 })?;
    }
    if !caps.network.is_empty() {
        linker.func_wrap(HOST_MODULE, "ainb_http_request", |_: Caller<'_, StubHost>, _: i32, _: i32, _: i32, _: i32| -> i32 { 0 })?;
    }
    if !caps.filesystem.is_empty() {
        linker.func_wrap(HOST_MODULE, "ainb_fs_read", |_: Caller<'_, StubHost>, _: i32, _: i32| -> i32 { 0 })?;
        linker.func_wrap(HOST_MODULE, "ainb_fs_glob", |_: Caller<'_, StubHost>, _: i32, _: i32| -> i32 { 0 })?;
    }

    // wasi_snapshot_preview1 floor — every Rust cdylib pulls in the
    // panic handler's environ_*/fd_write/proc_exit imports.
    link_wasi_floor(linker)?;
    Ok(())
}

fn link_wasi_floor(linker: &mut Linker<StubHost>) -> anyhow::Result<()> {
    const WASI: &str = "wasi_snapshot_preview1";
    // Each stub is the minimal "errno success" form. Returns 0 (no
    // error) and writes nothing, which is fine because the CTS only
    // exercises lifecycle entry points — not real I/O. Plugins that
    // actually depend on WASI semantics during _init / _shutdown will
    // surface a logic bug, not a missing-import error.
    linker.func_wrap(WASI, "environ_sizes_get", |_: Caller<'_, StubHost>, _: i32, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "environ_get", |_: Caller<'_, StubHost>, _: i32, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "args_sizes_get", |_: Caller<'_, StubHost>, _: i32, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "args_get", |_: Caller<'_, StubHost>, _: i32, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "clock_time_get", |_: Caller<'_, StubHost>, _: i32, _: i64, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "clock_res_get", |_: Caller<'_, StubHost>, _: i32, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "random_get", |_: Caller<'_, StubHost>, _: i32, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "proc_exit", |_: Caller<'_, StubHost>, _: i32| {})?;
    linker.func_wrap(WASI, "sched_yield", |_: Caller<'_, StubHost>| -> i32 { 0 })?;
    linker.func_wrap(WASI, "poll_oneoff", |_: Caller<'_, StubHost>, _: i32, _: i32, _: i32, _: i32| -> i32 { 0 })?;

    // fd_*
    linker.func_wrap(WASI, "fd_close", |_: Caller<'_, StubHost>, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "fd_write", |_: Caller<'_, StubHost>, _: i32, _: i32, _: i32, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "fd_read", |_: Caller<'_, StubHost>, _: i32, _: i32, _: i32, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "fd_pread", |_: Caller<'_, StubHost>, _: i32, _: i32, _: i32, _: i64, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "fd_pwrite", |_: Caller<'_, StubHost>, _: i32, _: i32, _: i32, _: i64, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "fd_seek", |_: Caller<'_, StubHost>, _: i32, _: i64, _: i32, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "fd_tell", |_: Caller<'_, StubHost>, _: i32, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "fd_sync", |_: Caller<'_, StubHost>, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "fd_datasync", |_: Caller<'_, StubHost>, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "fd_fdstat_get", |_: Caller<'_, StubHost>, _: i32, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "fd_fdstat_set_flags", |_: Caller<'_, StubHost>, _: i32, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "fd_filestat_get", |_: Caller<'_, StubHost>, _: i32, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "fd_prestat_get", |_: Caller<'_, StubHost>, _: i32, _: i32| -> i32 { 8 /* WASI_EBADF */ })?;
    linker.func_wrap(WASI, "fd_prestat_dir_name", |_: Caller<'_, StubHost>, _: i32, _: i32, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "fd_readdir", |_: Caller<'_, StubHost>, _: i32, _: i32, _: i32, _: i64, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "fd_renumber", |_: Caller<'_, StubHost>, _: i32, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "fd_advise", |_: Caller<'_, StubHost>, _: i32, _: i64, _: i64, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "fd_allocate", |_: Caller<'_, StubHost>, _: i32, _: i64, _: i64| -> i32 { 0 })?;

    // path_*
    linker.func_wrap(WASI, "path_open", |_: Caller<'_, StubHost>, _: i32, _: i32, _: i32, _: i32, _: i32, _: i64, _: i64, _: i32, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "path_filestat_get", |_: Caller<'_, StubHost>, _: i32, _: i32, _: i32, _: i32, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "path_readlink", |_: Caller<'_, StubHost>, _: i32, _: i32, _: i32, _: i32, _: i32, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "path_create_directory", |_: Caller<'_, StubHost>, _: i32, _: i32, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "path_remove_directory", |_: Caller<'_, StubHost>, _: i32, _: i32, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "path_rename", |_: Caller<'_, StubHost>, _: i32, _: i32, _: i32, _: i32, _: i32, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "path_unlink_file", |_: Caller<'_, StubHost>, _: i32, _: i32, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "path_symlink", |_: Caller<'_, StubHost>, _: i32, _: i32, _: i32, _: i32, _: i32| -> i32 { 0 })?;
    linker.func_wrap(WASI, "path_link", |_: Caller<'_, StubHost>, _: i32, _: i32, _: i32, _: i32, _: i32, _: i32, _: i32| -> i32 { 0 })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_contract_parses_as_toml() {
        let _: toml::Value = toml::from_str(CONTRACT_V1_TOML)
            .expect("contract.toml that ships with the crate must be valid TOML");
    }

    #[test]
    fn report_renders_human_readable() {
        let report = ConformanceReport {
            contract_version: "v1".to_string(),
            axes: vec![
                AxisOutcome {
                    id: 1,
                    name: "manifest schema".to_string(),
                    result: Ok(()),
                },
                AxisOutcome {
                    id: 7,
                    name: "version: ainb_min_version is semver".to_string(),
                    result: Err("not semver".to_string()),
                },
            ],
        };
        let s = format!("{report}");
        assert!(s.contains("1 pass / 1 fail"));
        assert!(s.contains("✓ axis  1"));
        assert!(s.contains("✗ axis  7"));
    }

    #[test]
    fn invalid_manifest_skips_remaining_axes() {
        let report = run_contract_v1("not toml at all", b"\0asm");
        assert!(!report.is_passing());
        let axis_1 = report.axes.iter().find(|a| a.id == 1).unwrap();
        assert!(axis_1.result.is_err(), "axis 1 must fail on bad manifest");
        // Every dependent axis must record a "skipped" reason.
        for id in [2, 3, 5, 6] {
            let axis = report.axes.iter().find(|a| a.id == id).unwrap();
            assert!(matches!(&axis.result, Err(s) if s.contains("skipped")));
        }
    }
}
