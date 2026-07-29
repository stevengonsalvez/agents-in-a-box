//! Phase 2 of the contract-testing hardening plan (tracking issue #500): a
//! FROZEN GOLDEN MATRIX over [`Runner::provider_command`], plus mutation proofs
//! that the comparison actually bites.
//!
//! # Why this file exists
//!
//! Hangar once shipped a full e2e suite that "proved" dispatch routing while the
//! daemon had never invoked a real agent CLI, because every one of those tests
//! used a fake script and NOTHING asserted the invocation SHAPE. Roughly 120
//! hand-written argv assertions have since accumulated in `tests/runner_*.rs`
//! and the crate's unit tests; they encode INTENT ("interactive must never be
//! print-and-exit"). What was missing was a single artifact a reviewer can READ
//! the exact argv, per provider, per mode, so that a silent change to what we
//! exec shows up as a legible diff in a pull request.
//!
//! This test freezes that. It does NOT replace the hand-written assertions: they
//! encode intent, the golden encodes shape, and both are wanted.
//!
//! # No production code is involved beyond the existing seam
//!
//! `Runner::provider_command(backend, invocation, mode) -> (PathBuf, Vec<String>)`
//! already returns the program and argv WITHOUT spawning. This file only calls
//! it. Every mutation proof below perturbs the RENDERED argv inside the test:
//! never the daemon, so the proofs cost nothing at runtime and cannot drift the
//! shipped shape.
//!
//! # Regenerating the goldens
//!
//! ```sh
//! UPDATE_GOLDEN=1 cargo test -p ainb-hangar-daemon --test argv_golden_matrix
//! ```
//!
//! That REWRITES `tests/golden/argv/<backend>.txt` from the current code and
//! passes. Only ever do this when you INTENDED to change the invocation shape,
//! and read the resulting diff before committing it: an unreviewed regeneration
//! turns this contract back into the fake gate it was written to replace.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ainb_hangar_daemon::runner::{Backend, Mode, ProviderInvocation, Runner, RunnerConfig};

/// The env var that rewrites the goldens instead of comparing against them.
const UPDATE_GOLDEN_VAR: &str = "UPDATE_GOLDEN";

/// The brief every case carries. Rendered as [`PROMPT_PLACEHOLDER`] so the
/// golden stays stable and readable; its POSITION in the argv (and the
/// `--` separator guarding it) is the part under contract.
const PROMPT: &str = "hangar argv golden contract brief";
/// What [`PROMPT`] is rendered as in the golden.
const PROMPT_PLACEHOLDER: &str = "<PROMPT>";

/// The model used by the `model=pinned` half of the matrix.
const PINNED_MODEL: &str = "pinned-model-id";

/// The extra provider CLI args used by the `cli_args=present` half of the
/// matrix.
///
/// Deliberately synthetic rather than a real per-provider flag (`--full-auto`,
/// `--add-dir`, …): `cli_args` is appended VERBATIM by every spec, so the spec
/// never interprets them. What the matrix must freeze is their PLACEMENT:
/// after the model flag, and (for claude / codex) before the `--` separator that
/// protects the brief. Synthetic tokens make a placement regression obvious and
/// avoid implying a flag is valid for a provider it is not.
const EXTRA_CLI_ARGS: [&str; 2] = ["--extra-flag", "--extra-key=extra-value"];

/// One case in the matrix.
struct Case {
    /// Which provider binary is being invoked.
    backend: Backend,
    /// Headless (captured, print-and-exit) or Interactive (attachable tmux).
    mode: Mode,
    /// `true` when the agent pinned a model, `false` for the provider default.
    model_pinned: bool,
    /// `true` when the agent supplied extra provider CLI args.
    cli_args_present: bool,
}

impl Case {
    /// The stable header this case is filed under in the golden.
    fn label(&self) -> String {
        format!(
            "{} | {} | model={} | cli_args={}",
            backend_label(self.backend),
            mode_label(self.mode),
            if self.model_pinned {
                "pinned"
            } else {
                "default"
            },
            if self.cli_args_present {
                "present"
            } else {
                "absent"
            },
        )
    }

    /// The invocation this case feeds to `provider_command`.
    fn invocation(&self) -> ProviderInvocation {
        ProviderInvocation {
            prompt: PROMPT.to_string(),
            model: self.model_pinned.then(|| PINNED_MODEL.to_string()),
            cli_args: if self.cli_args_present {
                EXTRA_CLI_ARGS.iter().map(|a| (*a).to_string()).collect()
            } else {
                Vec::new()
            },
        }
    }
}

/// Exhaustive, wildcard-free so adding a [`Backend`] variant fails to compile
/// here rather than silently escaping the matrix.
const fn backend_label(backend: Backend) -> &'static str {
    match backend {
        Backend::Claude => "claude",
        Backend::Codex => "codex",
        Backend::Copilot => "copilot",
    }
}

/// Exhaustive, wildcard-free for the same reason as [`backend_label`].
const fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Headless => "headless",
        Mode::Interactive => "interactive",
    }
}

/// The placeholder each provider's resolved binary path is rendered as.
///
/// The real path comes from `RunnerConfig` (a PATH lookup in production, a temp
/// dir in tests), so it is machine-specific and must be normalised, but it is
/// normalised, not omitted: a route that spawns the WRONG provider binary still
/// shows up as a changed placeholder.
const fn program_placeholder(backend: Backend) -> &'static str {
    match backend {
        Backend::Claude => "<CLAUDE_BIN>",
        Backend::Codex => "<CODEX_BIN>",
        Backend::Copilot => "<COPILOT_BIN>",
    }
}

/// Every case for one backend: mode x model x cli_args, in a fixed order so the
/// golden diff stays stable.
fn cases_for(backend: Backend) -> Vec<Case> {
    let mut cases = Vec::new();
    for mode in [Mode::Headless, Mode::Interactive] {
        for model_pinned in [false, true] {
            for cli_args_present in [false, true] {
                cases.push(Case {
                    backend,
                    mode,
                    model_pinned,
                    cli_args_present,
                });
            }
        }
    }
    cases
}

/// Every backend in the matrix, in golden-file order.
const BACKENDS: [Backend; 3] = [Backend::Claude, Backend::Codex, Backend::Copilot];

/// A runner whose provider paths are distinct, deterministic sentinels.
///
/// They are never executed, `provider_command` builds the command without
/// spawning, so they need not exist on disk. Distinct values mean a mis-routed
/// backend renders the wrong program line instead of coincidentally matching.
fn golden_runner() -> Runner {
    Runner::new(RunnerConfig {
        claude_path: PathBuf::from("/golden/bin/claude"),
        codex_path: PathBuf::from("/golden/bin/codex"),
        copilot_path: PathBuf::from("/golden/bin/copilot"),
        max_runtime: Duration::from_secs(60),
        tail_lines: 50,
        sandbox: true,
    })
}

/// Render ONE case block: a header naming it, the program, then the argv one
/// token per line so a PR diff points at the exact token that moved.
///
/// Takes the argv by reference rather than recomputing it, so the mutation
/// proofs below can render a PERTURBED argv through the very same formatter the
/// real comparison uses. A mutation proof that rendered through a different code
/// path would prove nothing about the real gate.
fn render_block(label: &str, backend: Backend, argv: &[String]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "=== case: {label}");
    let _ = writeln!(out, "program: {}", program_placeholder(backend));
    let _ = writeln!(out, "argv:");
    if argv.is_empty() {
        // An empty argv MUST render as a visible line, never as nothing. A
        // vacuous "no tokens" block that formats to the empty string is exactly
        // the failure this whole file exists to make impossible: it would look
        // like a passing case while the daemon exec'd a bare binary.
        let _ = writeln!(out, "  (EMPTY ARGV)");
    }
    for token in argv {
        let rendered = if token == PROMPT {
            PROMPT_PLACEHOLDER
        } else {
            token.as_str()
        };
        let _ = writeln!(out, "  {rendered}");
    }
    out
}

/// The full golden document for one backend, rendered from live production
/// output.
fn render_backend(runner: &Runner, backend: Backend) -> String {
    let name = backend_label(backend);
    let mut out = String::new();
    let _ = writeln!(out, "# ARGV GOLDEN: {name}");
    let _ = writeln!(out, "#");
    let _ = writeln!(
        out,
        "# Frozen output of Runner::provider_command for every {name} case in the"
    );
    let _ = writeln!(
        out,
        "# matrix: mode x model x cli_args. One token per line; <PROMPT> and"
    );
    let _ = writeln!(
        out,
        "# <*_BIN> are normalised so the file is stable across machines."
    );
    let _ = writeln!(out, "#");
    let _ = writeln!(
        out,
        "# Regenerate with UPDATE_GOLDEN=1 cargo test -p ainb-hangar-daemon \\"
    );
    let _ = writeln!(
        out,
        "#   --test argv_golden_matrix    (only when the change is INTENDED)."
    );

    for case in cases_for(backend) {
        let (_program, argv) = runner.provider_command(backend, &case.invocation(), case.mode);
        out.push('\n');
        out.push_str(&render_block(&case.label(), backend, &argv));
    }
    out
}

/// Where a backend's golden lives.
fn golden_path(backend: Backend) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/argv")
        .join(format!("{}.txt", backend_label(backend)))
}

/// Compare a rendered document against its golden.
///
/// `Ok(())` when identical; `Err(unified_diff)` otherwise. This is THE gate:
/// the mutation proofs assert against this function's return value, so a change
/// that made it lenient would fail its own proofs.
fn compare(golden: &str, actual: &str) -> Result<(), String> {
    if golden == actual {
        return Ok(());
    }
    Err(unified_diff(golden, actual))
}

/// A line-oriented unified diff, computed with a plain LCS.
///
/// Hand-rolled rather than pulled from a crate: the documents are a few hundred
/// lines and adding a dependency to print a failure message is not worth it.
fn unified_diff(golden: &str, actual: &str) -> String {
    let a: Vec<&str> = golden.lines().collect();
    let b: Vec<&str> = actual.lines().collect();

    // lcs[i][j] = length of the longest common subsequence of a[i..] and b[j..].
    let mut lcs = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    let mut out = String::from("--- golden\n+++ actual\n");
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            let _ = writeln!(out, "  {}", a[i]);
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            let _ = writeln!(out, "- {}", a[i]);
            i += 1;
        } else {
            let _ = writeln!(out, "+ {}", b[j]);
            j += 1;
        }
    }
    while i < a.len() {
        let _ = writeln!(out, "- {}", a[i]);
        i += 1;
    }
    while j < b.len() {
        let _ = writeln!(out, "+ {}", b[j]);
        j += 1;
    }
    out
}

/// Whether the caller asked to rewrite the goldens.
fn update_requested() -> bool {
    std::env::var_os(UPDATE_GOLDEN_VAR).is_some_and(|v| !v.is_empty() && v != "0")
}

// ---------------------------------------------------------------------------
// The contract
// ---------------------------------------------------------------------------

/// THE GATE: every provider's full argv matrix matches its frozen golden.
///
/// Any change to what the daemon exec's (a flag added, dropped or reordered, or
/// one mode's shape quietly converging on the other's) fails here with a unified
/// diff naming the exact token.
#[test]
fn provider_argv_matrix_matches_golden() {
    let runner = golden_runner();
    let updating = update_requested();
    let mut failures = Vec::new();

    for backend in BACKENDS {
        let actual = render_backend(&runner, backend);
        let path = golden_path(backend);

        if updating {
            std::fs::create_dir_all(path.parent().expect("golden dir")).expect("create golden dir");
            std::fs::write(&path, &actual).expect("write golden");
            continue;
        }

        let golden = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "missing golden {}: {e}\n\
                 Create it deliberately with {UPDATE_GOLDEN_VAR}=1 cargo test -p \
                 ainb-hangar-daemon --test argv_golden_matrix",
                path.display()
            )
        });

        if let Err(diff) = compare(&golden, &actual) {
            failures.push(format!(
                "\n{} argv drifted from {}:\n{diff}",
                backend_label(backend),
                path.display()
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "provider argv no longer matches the frozen contract.\n{}\n\
         If this change is INTENDED, regenerate with {UPDATE_GOLDEN_VAR}=1 and \
         review the diff before committing.",
        failures.join("")
    );
}

/// The matrix must stay exhaustive: 3 backends x 2 modes x 2 model states x
/// 2 cli_args states, with no duplicate case labels.
///
/// Without this, silently dropping a loop would shrink the contract while the
/// golden test stayed green against a shrunken golden.
#[test]
fn matrix_covers_every_backend_mode_and_option_axis() {
    let mut labels = Vec::new();
    for backend in BACKENDS {
        let cases = cases_for(backend);
        assert_eq!(
            cases.len(),
            8,
            "{} lost a case axis",
            backend_label(backend)
        );
        labels.extend(cases.iter().map(Case::label));
    }
    assert_eq!(labels.len(), 24, "matrix is not 3 x 2 x 2 x 2");

    let mut unique = labels.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        labels.len(),
        "duplicate case labels: {labels:?}"
    );
}

/// The golden files must be present and non-trivial, so a deleted or emptied
/// golden cannot make the gate vacuously pass.
#[test]
fn golden_files_exist_and_are_populated() {
    if update_requested() {
        // Under UPDATE_GOLDEN the files are being (re)written by a sibling test
        // in the same binary, and cargo runs tests in parallel, reading them
        // here would race the writer. Regeneration is not the run that gates.
        return;
    }
    for backend in BACKENDS {
        let path = golden_path(backend);
        let body = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("golden {} unreadable: {e}", path.display()));
        let cases = body.lines().filter(|l| l.starts_with("=== case: ")).count();
        assert_eq!(
            cases,
            8,
            "golden {} should hold 8 case blocks, found {cases}",
            path.display()
        );
    }
}

// ---------------------------------------------------------------------------
// Mutation proofs
//
// A golden test nobody has seen fail is not a test. These perturb the RENDERED
// argv (never production code) and assert the comparison returns a failure. If
// the gate is ever weakened into an always-Ok stub, these go red.
// ---------------------------------------------------------------------------

/// Baseline for the mutation proofs: the comparison accepts identical input.
///
/// Without this, "compare always errors" would satisfy every proof below.
#[test]
fn mutation_baseline_identical_input_compares_clean() {
    let runner = golden_runner();
    let doc = render_backend(&runner, Backend::Claude);
    assert!(
        compare(&doc, &doc).is_ok(),
        "comparison rejected identical input, so the mutation proofs prove nothing"
    );
}

/// The claude headless argv rendered from live production output, plus its
/// label, the substrate the mutations below perturb.
fn claude_headless_argv() -> (String, Vec<String>) {
    let runner = golden_runner();
    let case = Case {
        backend: Backend::Claude,
        mode: Mode::Headless,
        model_pinned: false,
        cli_args_present: false,
    };
    let (_program, argv) = runner.provider_command(Backend::Claude, &case.invocation(), case.mode);
    (case.label(), argv)
}

/// MUTATION 1 (flipped boolean flag): swapping `--dangerously-skip-permissions`
/// for a narrower `--permission-mode acceptEdits` turns the gate red.
///
/// This is the measured regression the spec's own docs warn about: the narrower
/// mode DENIES `Bash`, so a task that must run a build is denied, exits 0, and
/// is scored `done` over half-finished work. It must never land silently.
#[test]
fn mutation_flipped_permission_flag_turns_golden_red() {
    let (label, argv) = claude_headless_argv();
    let expected = render_block(&label, Backend::Claude, &argv);

    let mut mutated = argv.clone();
    let idx = mutated
        .iter()
        .position(|t| t == "--dangerously-skip-permissions")
        .expect("claude headless argv must carry --dangerously-skip-permissions");
    mutated[idx] = "--permission-mode".to_string();
    mutated.insert(idx + 1, "acceptEdits".to_string());

    let actual = render_block(&label, Backend::Claude, &mutated);
    let diff =
        compare(&expected, &actual).expect_err("flipping the permission flag must be caught");

    println!("--- MUTATION 1: flipped boolean flag ---\n{diff}");
    assert!(diff.contains("-   --dangerously-skip-permissions"));
    assert!(diff.contains("+   --permission-mode"));
}

/// MUTATION 2 (dropped token): removing a single argv token turns the gate red.
///
/// Drops `--verbose`, the flag that makes claude's `stream-json` actually emit
/// per-event lines. Without it the runner's structured terminal never arrives
/// and a clean exit is misread as contract drift.
#[test]
fn mutation_dropped_argv_token_turns_golden_red() {
    let (label, argv) = claude_headless_argv();
    let expected = render_block(&label, Backend::Claude, &argv);

    let mut mutated = argv.clone();
    let idx = mutated
        .iter()
        .position(|t| t == "--verbose")
        .expect("claude headless argv must carry --verbose");
    mutated.remove(idx);

    let actual = render_block(&label, Backend::Claude, &mutated);
    let diff = compare(&expected, &actual).expect_err("dropping an argv token must be caught");

    println!("--- MUTATION 2: dropped argv token ---\n{diff}");
    assert!(diff.contains("-   --verbose"));
    assert_eq!(
        mutated.len(),
        argv.len() - 1,
        "the mutation must actually drop exactly one token"
    );
}

/// MUTATION 3 (empty argv): rendering NO tokens at all turns the gate red.
///
/// This is the vacuous-pass case that makes a fake test look green, a shape
/// that produces nothing to compare must be the loudest failure, not the
/// quietest. The block renders `(EMPTY ARGV)` precisely so it cannot format to
/// the empty string and slip through.
#[test]
fn mutation_empty_argv_turns_golden_red() {
    let (label, argv) = claude_headless_argv();
    let expected = render_block(&label, Backend::Claude, &argv);

    let actual = render_block(&label, Backend::Claude, &[]);
    let diff = compare(&expected, &actual).expect_err("an empty argv must be caught");

    println!("--- MUTATION 3: empty argv ---\n{diff}");
    assert!(diff.contains("+   (EMPTY ARGV)"));
    assert!(diff.contains("-   --dangerously-skip-permissions"));
    assert!(
        !actual.trim().is_empty(),
        "an empty argv must still render a visible block"
    );
}

/// MUTATION 4 (mode convergence): giving the INTERACTIVE claude argv the
/// headless print flag turns the gate red.
///
/// `-p` is "print response and exit". In an attachable tmux pane it hands the
/// operator a dead terminal. The hand-written
/// `interactive_task_mode_never_produces_a_print_and_exit_argv` asserts the
/// intent; this asserts the golden would CATCH the drift.
#[test]
fn mutation_interactive_gaining_print_flag_turns_golden_red() {
    let runner = golden_runner();
    let case = Case {
        backend: Backend::Claude,
        mode: Mode::Interactive,
        model_pinned: false,
        cli_args_present: false,
    };
    let (_program, argv) =
        runner.provider_command(Backend::Claude, &case.invocation(), Mode::Interactive);
    assert!(
        !argv.iter().any(|t| t == "-p"),
        "the interactive argv must not already carry -p"
    );

    let expected = render_block(&case.label(), Backend::Claude, &argv);
    let mut mutated = argv.clone();
    mutated.insert(0, "-p".to_string());
    let actual = render_block(&case.label(), Backend::Claude, &mutated);

    let diff =
        compare(&expected, &actual).expect_err("an interactive argv gaining -p must be caught");
    println!("--- MUTATION 4: interactive gains the print flag ---\n{diff}");
    assert!(diff.contains("+   -p"));
}
