//! Error types shared across `ainb-core`.

use std::path::PathBuf;

/// Errors raised by the core library.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("invalid unit URI: {0}")]
    InvalidUri(String),

    #[error("unknown source type `{0}` — expected one of: gh, git, gist, https, local, npm, marketplace")]
    UnknownSourceType(String),

    #[error("manifest at {path:?} is invalid: {message}")]
    InvalidManifest { path: PathBuf, message: String },

    #[error("lockfile at {path:?} is invalid: {message}")]
    InvalidLockfile { path: PathBuf, message: String },

    #[error("source `{0}` not found in manifest")]
    SourceNotFound(String),

    #[error("source `{0}` already exists in manifest")]
    SourceAlreadyExists(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("yaml error: {0}")]
    Yaml(#[from] serde_yaml_ng::Error),
}

/// Convenience result alias used throughout `ainb-core`.
pub type Result<T> = std::result::Result<T, CoreError>;
