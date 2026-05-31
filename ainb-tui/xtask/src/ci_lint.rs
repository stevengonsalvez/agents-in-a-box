//! `cargo xtask ci-lint` — assert the *real* CI contract for the Hangar
//! end-to-end job.
//!
//! This deliberately mirrors the conventions ACTUALLY in
//! `.github/workflows/ci.yml` (the `ainb-hooks` per-feature job), NOT the stale
//! P9 plan. In particular it does **not** require a `cargo clippy -D warnings`
//! step — the workspace carries heavy pre-existing clippy debt and CI gates on
//! `cargo fmt --check` only. Adding a workspace-clippy gate would red the whole
//! repo's CI for unrelated reasons, so this linter forbids assuming it exists.
//!
//! Asserted contract for the `hangar-e2e` job:
//!   * the job exists,
//!   * its OS matrix contains BOTH `ubuntu-latest` and `macos-latest`,
//!   * tmux is installed (a step that runs `apt-get install … tmux` on Linux)
//!     BEFORE the step that runs the tripwire script,
//!   * a step runs `scripts/hangar/run_all_tripwires.sh`.
//!
//! Run: `cargo xtask ci-lint`.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use serde_yaml::Value;

/// Entry point for the `ci-lint` subcommand. Resolves the workflow file from the
/// repo root (the cargo workspace's parent) and validates the Hangar contract.
pub fn run() -> Result<()> {
    let ci_yml = ci_yml_path()?;
    let text = fs::read_to_string(&ci_yml).with_context(|| format!("read {}", ci_yml.display()))?;
    lint_ci_yaml(&text).with_context(|| format!("ci-lint {}", ci_yml.display()))?;
    println!("[xtask] ci-lint OK — hangar-e2e job satisfies the real CI contract");
    Ok(())
}

/// `<workspace>/xtask` → `<workspace>` → `<repo_root>/.github/workflows/ci.yml`.
/// The cargo workspace is `ainb-tui/`; `.github` lives at the repo root, one
/// level above.
fn ci_yml_path() -> Result<PathBuf> {
    let xtask_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace =
        xtask_dir.parent().ok_or_else(|| anyhow!("xtask manifest dir has no parent"))?;
    let repo_root = workspace
        .parent()
        .ok_or_else(|| anyhow!("workspace dir has no parent (repo root)"))?;
    Ok(repo_root.join(".github/workflows/ci.yml"))
}

/// Pure validator over the YAML text so it is unit-testable without touching the
/// filesystem.
pub fn lint_ci_yaml(text: &str) -> Result<()> {
    let doc: Value = serde_yaml::from_str(text).context("parse ci.yml as YAML")?;

    let job = doc.get("jobs").and_then(|j| j.get("hangar-e2e")).ok_or_else(|| {
        anyhow!("missing `jobs.hangar-e2e` — the Hangar e2e CI job is not defined")
    })?;

    assert_matrix_both_os(job)?;
    assert_tmux_before_tripwires(job)?;

    Ok(())
}

/// The OS matrix must include both runners. We accept either the flat
/// `strategy.matrix.os: [ubuntu-latest, macos-latest]` form (what the existing
/// `ainb-hooks`/`test` jobs use) or the `strategy.matrix.include[].os` form.
fn assert_matrix_both_os(job: &Value) -> Result<()> {
    let matrix = job
        .get("strategy")
        .and_then(|s| s.get("matrix"))
        .ok_or_else(|| anyhow!("hangar-e2e has no `strategy.matrix`"))?;

    let mut oses: Vec<String> = Vec::new();
    if let Some(seq) = matrix.get("os").and_then(Value::as_sequence) {
        oses.extend(seq.iter().filter_map(|v| v.as_str().map(str::to_owned)));
    }
    if let Some(includes) = matrix.get("include").and_then(Value::as_sequence) {
        for inc in includes {
            if let Some(os) = inc.get("os").and_then(Value::as_str) {
                oses.push(os.to_owned());
            }
        }
    }

    for required in ["ubuntu-latest", "macos-latest"] {
        if !oses.iter().any(|o| o == required) {
            bail!(
                "hangar-e2e matrix is missing `{required}` (found: {oses:?}); \
                 both ubuntu-latest and macos-latest are required"
            );
        }
    }
    Ok(())
}

/// Walk the job's steps in order: a tmux-install step (Linux `apt-get install …
/// tmux`) must appear BEFORE the step that runs `run_all_tripwires.sh`. Also
/// asserts the tripwire-running step exists at all.
fn assert_tmux_before_tripwires(job: &Value) -> Result<()> {
    let steps = job
        .get("steps")
        .and_then(Value::as_sequence)
        .ok_or_else(|| anyhow!("hangar-e2e has no `steps` sequence"))?;

    let mut tmux_idx: Option<usize> = None;
    let mut tripwire_idx: Option<usize> = None;

    for (i, step) in steps.iter().enumerate() {
        let run = step.get("run").and_then(Value::as_str).unwrap_or("");
        if tmux_idx.is_none() && run.contains("apt-get") && run.contains("tmux") {
            tmux_idx = Some(i);
        }
        if tripwire_idx.is_none() && run.contains("run_all_tripwires.sh") {
            tripwire_idx = Some(i);
        }
    }

    let tmux_idx = tmux_idx.ok_or_else(|| {
        anyhow!("hangar-e2e has no step installing tmux via `apt-get install … tmux`")
    })?;
    let tripwire_idx = tripwire_idx.ok_or_else(|| {
        anyhow!("hangar-e2e has no step running `scripts/hangar/run_all_tripwires.sh`")
    })?;

    if tmux_idx >= tripwire_idx {
        bail!(
            "hangar-e2e installs tmux (step {tmux_idx}) AFTER running the tripwires \
             (step {tripwire_idx}); tmux must be installed first"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal but faithful copy of the real `hangar-e2e` job shape this
    /// linter must accept: flat `os` matrix, Linux-only tmux install before the
    /// tripwire run, working-directory set on the test step.
    const GOOD: &str = r"
jobs:
  hangar-e2e:
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, macos-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - name: Install tmux (Linux only)
        if: matrix.os == 'ubuntu-latest'
        run: sudo apt-get update && sudo apt-get install -y tmux
      - name: Run hangar tripwires
        run: bash ../scripts/hangar/run_all_tripwires.sh
";

    #[test]
    fn accepts_the_real_contract() {
        lint_ci_yaml(GOOD).expect("good ci.yml must pass");
    }

    #[test]
    fn rejects_missing_job() {
        let yaml = "jobs:\n  test:\n    runs-on: ubuntu-latest\n";
        let err = lint_ci_yaml(yaml).unwrap_err().to_string();
        assert!(err.contains("hangar-e2e"), "got: {err}");
    }

    #[test]
    fn rejects_single_os() {
        let yaml = r"
jobs:
  hangar-e2e:
    strategy:
      matrix:
        os: [ubuntu-latest]
    steps:
      - run: sudo apt-get install -y tmux
      - run: bash scripts/hangar/run_all_tripwires.sh
";
        let err = lint_ci_yaml(yaml).unwrap_err().to_string();
        assert!(err.contains("macos-latest"), "got: {err}");
    }

    #[test]
    fn rejects_missing_tripwire_step() {
        let yaml = r"
jobs:
  hangar-e2e:
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest]
    steps:
      - run: sudo apt-get install -y tmux
      - run: cargo build
";
        let err = lint_ci_yaml(yaml).unwrap_err().to_string();
        assert!(err.contains("run_all_tripwires.sh"), "got: {err}");
    }

    #[test]
    fn rejects_tmux_after_tripwires() {
        let yaml = r"
jobs:
  hangar-e2e:
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest]
    steps:
      - run: bash scripts/hangar/run_all_tripwires.sh
      - run: sudo apt-get install -y tmux
";
        let err = lint_ci_yaml(yaml).unwrap_err().to_string();
        assert!(err.contains("must be installed first"), "got: {err}");
    }

    #[test]
    fn accepts_matrix_include_form() {
        let yaml = r"
jobs:
  hangar-e2e:
    strategy:
      matrix:
        include:
          - os: ubuntu-latest
          - os: macos-latest
    steps:
      - run: sudo apt-get install -y tmux
      - run: bash scripts/hangar/run_all_tripwires.sh
";
        lint_ci_yaml(yaml).expect("include-form matrix must pass");
    }
}
