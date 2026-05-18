//! Install plan + apply report types.
//!
//! A plan is a deterministic list of [`PlanOp`]s; rendering the plan
//! into a unified diff for the diff-and-confirm UX lives in
//! `ainb-diff`. `apply` writes the plan atomically (tmp + rename per
//! file) and returns an [`InstallReport`] whose `file_hashes` end up
//! in the lockfile.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use ainb_skill_core::UnitKind;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// One file-level operation produced by [`super::ToolAdapter::plan_install`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanOp {
    /// File does not exist on disk; will be written.
    Create {
        dst: PathBuf,
        contents: Vec<u8>,
    },
    /// File exists on disk; will be overwritten.
    Update {
        dst: PathBuf,
        previous: Vec<u8>,
        contents: Vec<u8>,
    },
    /// File exists on disk; will be removed (used by uninstall plans).
    Delete { dst: PathBuf },
}

impl PlanOp {
    pub fn destination(&self) -> &Path {
        match self {
            PlanOp::Create { dst, .. }
            | PlanOp::Update { dst, .. }
            | PlanOp::Delete { dst } => dst,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            PlanOp::Create { .. } => "create",
            PlanOp::Update { .. } => "update",
            PlanOp::Delete { .. } => "delete",
        }
    }
}

/// Combined plan for one (unit, tool) deployment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallPlan {
    pub tool: String,
    pub unit_uri: String,
    pub kind: UnitKind,
    /// Directory the deployment lives under (`install_root/skills/foo/`).
    pub unit_install_path: PathBuf,
    pub ops: Vec<PlanOp>,
}

impl InstallPlan {
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
    pub fn len(&self) -> usize {
        self.ops.len()
    }
}

/// Result of [`super::ToolAdapter::apply`]. Carries the install
/// path + per-file sha256 hashes so the lockfile can detect drift.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallReport {
    pub tool: String,
    pub unit_uri: String,
    pub path: PathBuf,
    /// Map of `file path relative to install_root` → `sha256:<hex>`.
    pub file_hashes: BTreeMap<String, String>,
}

/// RAII guard that removes a `.ainb-tmp` file on early return / panic
/// so an interrupted write doesn't leave stale debris in install
/// roots. `disarm()` is called once the rename to the final path
/// succeeds — the tmp file is consumed by rename at that point, so
/// there's nothing left for Drop to clean up.
struct TmpGuard {
    path: Option<std::path::PathBuf>,
}

impl TmpGuard {
    fn new(path: std::path::PathBuf) -> Self {
        Self { path: Some(path) }
    }
    fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for TmpGuard {
    fn drop(&mut self) {
        if let Some(p) = self.path.take() {
            let _ = fs::remove_file(&p);
        }
    }
}

/// Apply a plan atomically. Each Create/Update writes to a sibling
/// `<dst>.ainb-tmp` and renames; Delete removes the file. Failed
/// writes leave no debris — `TmpGuard` cleans up any `.ainb-tmp`
/// file whose rename never happened. Returns the per-file sha256
/// catalog the apply produced.
pub fn apply_plan(plan: &InstallPlan, install_root: &Path) -> anyhow::Result<InstallReport> {
    let mut hashes: BTreeMap<String, String> = BTreeMap::new();
    for op in &plan.ops {
        match op {
            PlanOp::Create { dst, contents } | PlanOp::Update { dst, contents, .. } => {
                if let Some(parent) = dst.parent() {
                    fs::create_dir_all(parent)?;
                }
                let tmp_name = dst
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "tmp".to_string());
                let tmp_path = dst
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join(format!(".{tmp_name}.ainb-tmp"));
                let mut guard = TmpGuard::new(tmp_path.clone());
                {
                    let mut f = fs::File::create(&tmp_path)?;
                    f.write_all(contents)?;
                    f.sync_all()?;
                }
                fs::rename(&tmp_path, dst)?;
                guard.disarm();

                let rel = relative_to(install_root, dst);
                hashes.insert(rel, format!("sha256:{}", hex_sha256(contents)));
            }
            PlanOp::Delete { dst } => {
                if dst.exists() {
                    fs::remove_file(dst)?;
                }
            }
        }
    }
    Ok(InstallReport {
        tool: plan.tool.clone(),
        unit_uri: plan.unit_uri.clone(),
        path: plan.unit_install_path.clone(),
        file_hashes: hashes,
    })
}

fn relative_to(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .map(|r| r.to_string_lossy().to_string())
        .unwrap_or_else(|_| p.to_string_lossy().to_string())
}

/// Lowercase hex SHA-256 of `bytes`. Surfaced as the shared hashing
/// primitive so `ainb doctor` can re-hash deployed files and compare
/// against the lockfile's recorded `sha256:` prefix without
/// duplicating the digest formatter.
pub fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op_create(dst: &Path, body: &str) -> PlanOp {
        PlanOp::Create {
            dst: dst.to_path_buf(),
            contents: body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn apply_creates_files_with_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let plan = InstallPlan {
            tool: "claude".into(),
            unit_uri: "gh:o/r@m/skills/foo".into(),
            kind: UnitKind::Skill,
            unit_install_path: dir.path().join("skills/foo"),
            ops: vec![
                op_create(&dir.path().join("skills/foo/SKILL.md"), "# hi\n"),
                op_create(&dir.path().join("skills/foo/assets/a.md"), "asset\n"),
            ],
        };
        let report = apply_plan(&plan, dir.path()).unwrap();
        assert!(dir.path().join("skills/foo/SKILL.md").exists());
        assert!(dir.path().join("skills/foo/assets/a.md").exists());
        assert_eq!(report.file_hashes.len(), 2);
        // Hashes are sha256: prefixed, 64 hex chars.
        for h in report.file_hashes.values() {
            assert!(h.starts_with("sha256:"), "got: {h}");
            assert_eq!(h.len(), 7 + 64);
        }
    }

    #[test]
    fn apply_update_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let dst = dir.path().join("skills/x/SKILL.md");
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        fs::write(&dst, b"old").unwrap();
        let plan = InstallPlan {
            tool: "claude".into(),
            unit_uri: "u".into(),
            kind: UnitKind::Skill,
            unit_install_path: dir.path().join("skills/x"),
            ops: vec![PlanOp::Update {
                dst: dst.clone(),
                previous: b"old".to_vec(),
                contents: b"new".to_vec(),
            }],
        };
        apply_plan(&plan, dir.path()).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"new");
    }

    #[test]
    fn apply_delete_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let dst = dir.path().join("skills/x/SKILL.md");
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        fs::write(&dst, b"x").unwrap();
        let plan = InstallPlan {
            tool: "claude".into(),
            unit_uri: "u".into(),
            kind: UnitKind::Skill,
            unit_install_path: dir.path().join("skills/x"),
            ops: vec![PlanOp::Delete { dst: dst.clone() }],
        };
        apply_plan(&plan, dir.path()).unwrap();
        assert!(!dst.exists());
    }
}
