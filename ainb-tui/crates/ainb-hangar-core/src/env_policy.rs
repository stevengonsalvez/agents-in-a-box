//! P5.3 — environment allowlist policy (pure, IO-free).
//!
//! Hangar spawns provider subprocesses with a **deny-by-default** environment.
//!
//! Only allowlisted keys (plus a built-in default set) pass through from the
//! daemon's ambient environment.
//!
//! A second, non-negotiable pass then strips a hardcoded family of
//! code-injection variables ([`DENY`]) — `LD_PRELOAD`, `DYLD_INSERT_LIBRARIES`,
//! `PYTHONPATH`, and friends — *even if the user added them to the allowlist*.
//! This reverses the DE finding §6 blocklist into an allowlist where deny
//! always wins.
//!
//! API keys (`ANTHROPIC_API_KEY`, …) are **never** governed here: they are
//! injected from the OS keychain at exec time via `Command::env`, not sourced
//! from the parent environment. This module only decides which *ambient* env
//! vars pass through.
//!
//! # Purity
//!
//! [`apply_policy`] takes an input [`HashMap`] and returns a new one.
//!
//! It never reads or mutates the process environment, which keeps the test
//! suite parallel-safe (no `std::env::set_var`; per
//! `reference_env_lock_for_parallel_tests`) and keeps this crate's no-IO
//! contract intact. The TOML loader and daemon wiring live in
//! `ainb-hangar-daemon`.

// The module doc intentionally leads with several short context paragraphs and
// a `# Purity` section; clippy's first-paragraph heuristic mis-anchors on the
// post-heading prose here. The content is already split into short paragraphs,
// so the lint adds no value for this module's narrative docs.
#![allow(clippy::too_long_first_doc_paragraph)]

use std::collections::{BTreeSet, HashMap};

/// The hardcoded code-injection deny family.
///
/// These names let an attacker (or a foot-gun config) load arbitrary code into
/// the provider subprocess (dynamic-linker preload, interpreter option
/// injection, startup-file hijacks). They are stripped on a mandatory second
/// pass *after* the allowlist, so even an explicit user allow entry cannot
/// re-enable them. Membership is exact-match (these are never glob patterns).
pub const DENY: &[&str] = &[
    "LD_PRELOAD",
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
    "PYTHONPATH",
    "NODE_OPTIONS",
    "BASH_ENV",
    "ENV",
    "GOPRIVATE",
    "RUBYOPT",
    "PERL5OPT",
    "JAVA_TOOL_OPTIONS",
];

/// The 12-entry built-in default allowlist.
///
/// Trailing-`*` entries (`LC_*`, `CLAUDE_*`) are prefix globs matched by
/// [`matches_allow`]; the rest are exact keys.
const DEFAULT_ALLOW: &[&str] = &[
    "HOME",
    "PATH",
    "TERM",
    "LANG",
    "USER",
    "SHELL",
    "LC_*",
    "COLUMNS",
    "LINES",
    "CLAUDE_*",
    "CODEX_HOME",
    "ANTHROPIC_BASE_URL",
];

/// An environment passthrough policy: a configurable allowlist plus the
/// always-on hardcoded [`DENY`] family.
///
/// Construct the operator-default policy via [`EnvPolicy::default`] (the 12
/// [`DEFAULT_ALLOW`] entries) and extend [`EnvPolicy::allow`] with user
/// overrides loaded from `~/.ainb/hangar/env.allow.toml`.
#[derive(Debug, Clone)]
pub struct EnvPolicy {
    /// Allowlisted env-var names. An entry ending in `*` is a prefix glob (e.g.
    /// `LC_*` matches `LC_ALL`, `LC_CTYPE`); every other entry is an exact key.
    pub allow: BTreeSet<String>,
    /// The hardcoded deny family — always [`DENY`]. Held as a field (rather than
    /// only a free constant) so a policy value carries its own complete rule set
    /// and renderers can show the locked entries.
    pub deny_hardcoded: &'static [&'static str],
}

impl Default for EnvPolicy {
    /// The operator default: the 12 built-in allow entries + the [`DENY`] family.
    fn default() -> Self {
        Self {
            allow: DEFAULT_ALLOW.iter().map(|s| (*s).to_string()).collect(),
            deny_hardcoded: DENY,
        }
    }
}

impl EnvPolicy {
    /// The built-in default allowlist as owned strings (for the loader to seed a
    /// fresh config / for the CLI to render the effective set).
    #[must_use]
    pub fn default_allow() -> BTreeSet<String> {
        DEFAULT_ALLOW.iter().map(|s| (*s).to_string()).collect()
    }

    /// Whether `key` is explicitly denied by the hardcoded family.
    #[must_use]
    pub fn is_denied(&self, key: &str) -> bool {
        self.deny_hardcoded.contains(&key)
    }

    /// Whether `key` is permitted by the allowlist (ignoring the deny pass).
    #[must_use]
    pub fn is_allowed(&self, key: &str) -> bool {
        self.allow.iter().any(|pat| matches_allow(pat, key))
    }
}

/// Match an allowlist `pattern` against an env-var `key`.
///
/// A pattern ending in `*` is a prefix glob: everything before the `*` must be a
/// prefix of `key` (so `LC_*` matches `LC_ALL` but not `LCD`). Any other pattern
/// is an exact, case-sensitive match. No regex — `*`-suffix is the only
/// metacharacter.
fn matches_allow(pattern: &str, key: &str) -> bool {
    pattern.strip_suffix('*').map_or_else(
        || pattern == key,
        |prefix| key.starts_with(prefix),
    )
}

/// Filter `parent_env` down to the keys this `policy` permits.
///
/// Two passes:
/// 1. **allow** — keep only keys matching an [`EnvPolicy::allow`] entry
///    (exact or `*`-suffix glob);
/// 2. **deny** — unconditionally drop any key in the hardcoded [`DENY`] family,
///    even if pass 1 (or the user's allowlist) admitted it.
///
/// Returns a fresh map; `parent_env` is never mutated and the process
/// environment is never touched.
#[must_use]
pub fn apply_policy<S: std::hash::BuildHasher>(
    policy: &EnvPolicy,
    parent_env: &HashMap<String, String, S>,
) -> HashMap<String, String> {
    parent_env
        .iter()
        .filter(|(k, _)| policy.is_allowed(k))
        .filter(|(k, _)| !policy.is_denied(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_requires_underscore_boundary() {
        assert!(matches_allow("LC_*", "LC_ALL"));
        assert!(!matches_allow("LC_*", "LCD"));
        assert!(matches_allow("HOME", "HOME"));
        assert!(!matches_allow("HOME", "HOMEPAGE"));
    }

    #[test]
    fn deny_is_the_full_family() {
        let p = EnvPolicy::default();
        for d in DENY {
            assert!(p.is_denied(d));
        }
        assert!(!p.is_denied("HOME"));
    }
}
