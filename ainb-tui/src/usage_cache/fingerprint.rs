// ABOUTME: Per-file fingerprint + change-detection for the usage cache.
// (size, mtime, suffix-hash) tuple; suffix hash catches the truncate+
// rewrite-to-same-size case the (mtime,size) tuple alone misses.

//! File fingerprinting for the persistent usage cache.

use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::store::CacheError;

/// How many trailing bytes feed into the suffix hash. 64 KiB chosen
/// over 4 KiB because Claude assistant-line bodies routinely exceed
/// 4 KiB (long tool results, large diffs); an in-place tail edit could
/// leave the last 4 KiB byte-identical and the cache would serve a
/// stale entry. blake3 runs at ~1 GB/s on modern hardware so the
/// extra 60 KiB is sub-millisecond per file.
pub const SUFFIX_HASH_BYTES: u64 = 65_536;

/// Per-file change-detection record stored in the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFingerprint {
    /// Total size of the file at last parse, in bytes.
    pub size_bytes: u64,
    /// File mtime at last parse, in nanos since UNIX epoch.
    pub mtime_nanos: i64,
    /// `blake3` over the trailing [`SUFFIX_HASH_BYTES`] bytes of the file
    /// (or the full file if smaller).
    pub suffix_blake3: [u8; 32],
}

impl FileFingerprint {
    /// Compute a fresh fingerprint for `path`.
    #[allow(dead_code)]
    pub fn from_path(path: &Path) -> Result<Self, CacheError> {
        let mut file = File::open(path).map_err(CacheError::Io)?;
        let meta = file.metadata().map_err(CacheError::Io)?;
        Self::from_open_file(&mut file, &meta)
    }

    /// Compute fingerprint from an already-open file + its metadata.
    pub fn from_open_file(file: &mut File, meta: &Metadata) -> Result<Self, CacheError> {
        let size_bytes = meta.len();
        let mtime_nanos = mtime_nanos_from_meta(meta);
        let suffix_blake3 = hash_suffix(file, size_bytes)?;
        Ok(Self {
            size_bytes,
            mtime_nanos,
            suffix_blake3,
        })
    }

    /// Build a fingerprint from raw column values (for sqlite reads). The
    /// blob must be exactly 32 bytes; on size mismatch we return `Schema`.
    pub fn from_columns(
        size_bytes: u64,
        mtime_nanos: i64,
        suffix_bytes: &[u8],
    ) -> Result<Self, CacheError> {
        let suffix_blake3: [u8; 32] = suffix_bytes
            .try_into()
            .map_err(|_| CacheError::Schema("suffix_blake3 column not 32 bytes".into()))?;
        Ok(Self {
            size_bytes,
            mtime_nanos,
            suffix_blake3,
        })
    }
}

/// What action the caller should take on the next parse for this file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FingerprintAction {
    /// File is byte-identical to last parse. Return cached blob.
    Unchanged,
    /// File grew. Caller should verify the prior tail still matches via
    /// [`verify_append_safe`] and, if so, parse from `from_offset` only.
    Append { from_offset: u64 },
    /// Anything else (size shrank, suffix mismatch, no prior fingerprint).
    /// Caller must full-re-parse.
    FullReparse,
}

/// Decide what to do given the prior cached fingerprint + the file's current
/// state on disk.
///
/// Pragmatic choices:
/// * Same `size_bytes` and same suffix hash → `Unchanged`. mtime is
///   ignored to tolerate sync tools that bump mtime without altering bytes,
///   and clock skew that makes mtime appear to regress.
/// * Same `size_bytes` but different suffix → `FullReparse`. The trailing
///   bytes were rewritten in place; we can't trust any prior offset.
/// * Size shrank → `FullReparse`. JSONL is append-only; truncation means
///   the file was rewritten and prior offsets are meaningless.
/// * Size grew → tentative `Append`. The byte-equality verification of the
///   prior tail happens in [`verify_append_safe`] because it needs disk I/O.
pub fn classify(
    prior: &FileFingerprint,
    current: &FileFingerprint,
    prior_offset: u64,
) -> FingerprintAction {
    if prior.size_bytes == current.size_bytes {
        if prior.suffix_blake3 == current.suffix_blake3 {
            return FingerprintAction::Unchanged;
        }
        return FingerprintAction::FullReparse;
    }

    if current.size_bytes < prior.size_bytes {
        return FingerprintAction::FullReparse;
    }

    FingerprintAction::Append {
        from_offset: prior_offset,
    }
}

/// Verify that, given a candidate `Append` action, the bytes that *were*
/// present at `prior_offset_bytes - SUFFIX_HASH_BYTES .. prior_offset_bytes`
/// in the prior parse still hash to `prior_suffix_blake3` in the current
/// file. `prior_offset_bytes` is the byte position where the previous parse
/// stopped — equal to the file size at that time (JSONL is append-only).
///
/// Returns `true` if append is safe; `false` means the prior tail was
/// rewritten and the caller must downgrade to `FullReparse`.
pub fn verify_append_safe(
    file: &mut File,
    prior_offset_bytes: u64,
    prior_suffix_blake3: &[u8; 32],
) -> Result<bool, CacheError> {
    if prior_offset_bytes == 0 {
        return Ok(true);
    }

    let window = SUFFIX_HASH_BYTES.min(prior_offset_bytes);
    let start = prior_offset_bytes - window;
    file.seek(SeekFrom::Start(start)).map_err(CacheError::Io)?;
    let mut buf = vec![0u8; window as usize];
    file.read_exact(&mut buf).map_err(CacheError::Io)?;
    let hash = blake3::hash(&buf);
    Ok(hash.as_bytes() == prior_suffix_blake3)
}

fn hash_suffix(file: &mut File, size_bytes: u64) -> Result<[u8; 32], CacheError> {
    if size_bytes == 0 {
        return Ok(*blake3::hash(&[]).as_bytes());
    }
    let window = SUFFIX_HASH_BYTES.min(size_bytes);
    let start = size_bytes - window;
    file.seek(SeekFrom::Start(start)).map_err(CacheError::Io)?;
    let mut buf = vec![0u8; window as usize];
    file.read_exact(&mut buf).map_err(CacheError::Io)?;
    Ok(*blake3::hash(&buf).as_bytes())
}

fn mtime_nanos_from_meta(meta: &Metadata) -> i64 {
    use std::time::UNIX_EPOCH;
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_nanos()).ok())
        .unwrap_or(0)
}
