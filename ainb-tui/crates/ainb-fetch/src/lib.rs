//! ainb-fetch — fetcher trait and cache layout.
//!
//! Each `<source-type>` resolves a [`Uri`](ainb_skill_core::Uri) to a local
//! checkout under `$AINB_HOME/cache/<source-name>/<short-sha>/` via an
//! implementation of [`Fetcher`]. The cache layout itself lives in
//! [`cache`]; concrete fetchers (git, http, local) plug in module-by-
//! module across the rest of the crate.

pub mod cache;
pub mod fetcher;
pub mod git;
pub mod http;
pub mod local;

pub use cache::{cache_path_for, short_sha};
pub use fetcher::{FetchError, FetchedRef, Fetcher};
pub use git::GitFetcher;
pub use http::HttpFetcher;
pub use local::LocalFetcher;
