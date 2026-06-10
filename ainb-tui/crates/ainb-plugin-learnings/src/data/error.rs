//! Typed error for the data layer.
//!
//! Every reader returns `Result<_, DataError>` so a malformed KB (truncated
//! graphml, corrupt community json, a missing file) surfaces as a structured
//! error instead of a panic — the UI can then render an empty/error state
//! rather than taking the whole plugin process down.
//!
//! The underlying-cause fields are named `detail` (not `source`) on purpose:
//! `thiserror` treats a field literally named `source` as a
//! `std::error::Error` cause, but we carry already-stringified causes, so the
//! neutral name keeps these plain data fields.

use thiserror::Error;

/// Failure modes of the learnings data readers.
#[derive(Debug, Error)]
pub enum DataError {
    /// Filesystem read failed (missing file/dir, permission, ...). Carries the
    /// path so the caller can report which KB location was unreadable.
    #[error("io error reading {path}: {detail}")]
    Io {
        /// The path that failed to read.
        path: String,
        /// Stringified underlying `std::io::Error`.
        detail: String,
    },

    /// A `.md` frontmatter block or `.entities.yaml` sidecar failed to parse.
    #[error("yaml parse error in {path}: {detail}")]
    Yaml {
        /// The file whose YAML was malformed.
        path: String,
        /// Stringified `serde_yaml` error.
        detail: String,
    },

    /// The frontmatter delimiters (`---` ... `---`) were missing or unbalanced.
    #[error("malformed frontmatter in {path}: {reason}")]
    Frontmatter {
        /// The `.md` file with the bad frontmatter.
        path: String,
        /// Human-readable reason.
        reason: String,
    },

    /// GraphML (XML) failed to parse — truncated, unbalanced tags, bad encoding.
    #[error("graphml parse error in {path}: {detail}")]
    GraphMl {
        /// The `*.graphml` file that failed.
        path: String,
        /// Stringified XML error.
        detail: String,
    },

    /// A JSON document (community reports / qmd output) failed to parse.
    #[error("json parse error in {path}: {detail}")]
    Json {
        /// The JSON file (or `<qmd output>` for shell results) that failed.
        path: String,
        /// Stringified `serde_json` error.
        detail: String,
    },

    /// Shelling `qmd` failed (binary missing, non-zero exit, ...).
    #[error("qmd subprocess failed: {0}")]
    Subprocess(String),
}
