//! Save / load the BSP [`LayoutSnapshot`] to disk.
//!
//! Persistence target: `~/.agents-in-a-box/layout.json`. Pure-fn here —
//! the host wires the actual path resolution in
//! `config::io::layout_persist_path` (follow-up bead) and calls
//! [`LayoutPersist::save`] on shutdown / [`LayoutPersist::load`] on
//! startup.
//!
//! Schema is JSON tagged by `version: u32`. Anything else surfaces as
//! [`PersistError::SchemaMismatch`]; the caller should fall back to
//! `LayoutNode::default_root()` and warn.
//!
//! Writes are atomic: `save` writes to a `*.tmp` sibling and renames
//! over the target so a crash mid-write can't corrupt the on-disk file.

use std::fs;
use std::io::Write;
use std::path::Path;

use thiserror::Error;

use crate::ui::bsp::LayoutSnapshot;

/// Current schema version for `layout.json`. Bump when the
/// `LayoutSnapshot` shape changes incompatibly.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum PersistError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error(
        "schema version mismatch: file has {got}, host expects {CURRENT_SCHEMA_VERSION}"
    )]
    SchemaMismatch { got: u32 },
}

pub struct LayoutPersist;

impl LayoutPersist {
    /// Serialise `snapshot` to `path` atomically. Returns the number of
    /// bytes written for caller diagnostics.
    pub fn save(snapshot: &LayoutSnapshot, path: &Path) -> Result<usize, PersistError> {
        let mut tmp = path.to_path_buf();
        let original_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("layout.json");
        tmp.set_file_name(format!("{original_name}.tmp"));

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let payload = serde_json::to_vec_pretty(snapshot)?;
        let len = payload.len();
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&payload)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)?;
        Ok(len)
    }

    /// Load and validate `path`. Returns `Ok(None)` when the file does
    /// not exist (first launch); `Err(SchemaMismatch)` when the file is
    /// readable JSON but a different schema version; `Err(Io/Json)` on
    /// other failures.
    pub fn load(path: &Path) -> Result<Option<LayoutSnapshot>, PersistError> {
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(path)?;
        let snapshot: LayoutSnapshot = serde_json::from_slice(&bytes)?;
        if snapshot.version != CURRENT_SCHEMA_VERSION {
            return Err(PersistError::SchemaMismatch {
                got: snapshot.version,
            });
        }
        Ok(Some(snapshot))
    }
}
