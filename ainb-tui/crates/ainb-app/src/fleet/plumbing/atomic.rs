// ABOUTME: Crash-safe atomic file write — temp-in-same-dir → fsync → rename → fsync(dir).
//
// Every durable artefact in the plumbing (status files, the per-parent inbox,
// the dead-letter sink) is rewritten through [`write_atomic`]. The contract is
// the POSIX-standard durable-replace dance:
//   1. write the new bytes to a temp file in the SAME directory as the target
//      (so the final rename is intra-filesystem and therefore atomic),
//   2. `fsync` the temp file's data + metadata to stable storage,
//   3. `rename` the temp over the target (atomic on POSIX — a concurrent reader
//      sees either the whole old file or the whole new file, never a torn one),
//   4. `fsync` the containing directory so the rename itself survives a crash.
//
// A reader therefore never observes a partial write, and once `write_atomic`
// returns Ok the bytes are durable across a power loss. This is the exactly-once
// foundation: the inbox can be rewritten under a lock with no risk of a torn
// JSONL line resurrecting a half-consumed completion.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

/// Atomically and durably replace `path` with `bytes`.
///
/// Creates the parent directory if missing. On success the bytes are fsync'd
/// and the rename is fsync'd, so the new content survives a crash. Concurrent
/// readers always see either the prior content or the full new content.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| Path::new(".").to_path_buf());
    fs::create_dir_all(&parent)
        .with_context(|| format!("creating parent dir {}", parent.display()))?;

    // Temp file in the SAME directory so the rename is atomic (same filesystem).
    let mut tmp = tempfile::Builder::new()
        .prefix(".ainb-tmp-")
        .tempfile_in(&parent)
        .with_context(|| format!("creating temp file in {}", parent.display()))?;
    tmp.write_all(bytes).context("writing bytes to temp file")?;
    tmp.flush().context("flushing temp file")?;
    // fsync the data + metadata before the rename so the new content is durable.
    tmp.as_file().sync_all().context("fsync temp file")?;

    // Atomic replace.
    tmp.persist(path)
        .map_err(|e| e.error)
        .with_context(|| format!("atomically renaming temp over {}", path.display()))?;

    // fsync the directory so the rename entry itself survives a crash.
    fsync_dir(&parent)?;
    Ok(())
}

/// Convenience wrapper: serialize `value` as pretty JSON and write it atomically.
pub fn write_atomic_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    let json = serde_json::to_vec_pretty(value).context("serializing value to JSON")?;
    write_atomic(path, &json)
}

/// fsync a directory handle so a rename/create inside it is durable. Best-effort
/// on platforms where opening a directory as a file is unsupported.
fn fsync_dir(dir: &Path) -> Result<()> {
    // Opening a directory read-only and fsync'ing it persists the rename on
    // POSIX. On systems that reject this (rare), degrade silently — the temp
    // file fsync above is the load-bearing durability guarantee.
    match File::open(dir) {
        Ok(f) => {
            let _ = f.sync_all();
            Ok(())
        }
        Err(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn writes_then_reads_back_exact_bytes() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("a.txt");
        write_atomic(&p, b"hello world").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"hello world");
    }

    #[test]
    fn creates_missing_parent_directories() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("deep").join("nested").join("a.json");
        assert!(!p.parent().unwrap().exists());
        write_atomic(&p, b"{}").unwrap();
        assert!(p.exists());
    }

    #[test]
    fn overwrite_replaces_prior_content_in_full() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("a.txt");
        write_atomic(&p, b"first-version-long").unwrap();
        write_atomic(&p, b"short").unwrap();
        // No torn read: must be exactly the new content, never a mix.
        assert_eq!(fs::read(&p).unwrap(), b"short");
    }

    #[test]
    fn leaves_no_temp_files_behind() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("a.txt");
        write_atomic(&p, b"x").unwrap();
        let strays: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with(".ainb-tmp-"))
            .collect();
        assert!(strays.is_empty(), "temp files leaked: {strays:?}");
    }

    #[test]
    fn json_helper_round_trips() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("v.json");
        write_atomic_json(&p, &json!({"k": 1, "s": "v"})).unwrap();
        let back: serde_json::Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
        assert_eq!(back["k"], 1);
        assert_eq!(back["s"], "v");
    }
}
