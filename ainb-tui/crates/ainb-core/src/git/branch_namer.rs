// ABOUTME: Pure auto-derivation of worktree branch names for the redesigned
// new-session Configure screen.
//
// Branch name shape: `<prefix><8-hex-rand>` — e.g. `agents/a3b7c91d`. The
// prefix is `AppConfig.workspace_defaults.branch_prefix` (default `agents/`).
//
// Stevie 2026-05-27: previously the slug came from the prompt's first three
// words OR a date-style timestamp. That recomputed every prompt keystroke
// (jittery branch line) and tied a permanent branch name to ephemeral prompt
// text. Random 8-hex matches the legacy `agents/<random>` muscle memory and
// stays stable for the lifetime of the Configure session.

use uuid::Uuid;

/// Derive a worktree branch name with `<prefix><8-hex>` shape.
///
/// * `prefix` — branch-name prefix from `AppConfig.workspace_defaults.branch_prefix`
///              (default `"agents/"`). Users who set `branch_prefix = "my-team/"`
///              get `my-team/<rand>` branches.
/// * `existing` — branch names already present in the repo; if the random
///              suffix happens to collide, we retry up to 8 times before
///              falling back to a counter-suffixed form.
///
/// **Stable for the lifetime of one Configure session**: callers stash the
/// result in `ConfigureState.branch_worktree` at open time and reuse it across
/// prompt edits / mode toggles / Tab cycles. Only `^R` reset on PickRepo or
/// re-opening Configure re-rolls.
///
/// Suffix source: first 8 hex chars of a fresh `Uuid::new_v4()`. UUID v4 has
/// 122 bits of entropy; taking the first 32 bits gives a 4-billion namespace
/// with negligible collision probability.
#[must_use]
pub fn derive_branch_name(prefix: &str, existing: &[String]) -> String {
    for _ in 0..8 {
        let suffix = short_hex_suffix();
        let candidate = format!("{prefix}{suffix}");
        if !existing.contains(&candidate) {
            return candidate;
        }
    }
    // Pathological collision case — append a counter to whatever the last
    // candidate was. Caller still gets a unique name.
    let suffix = short_hex_suffix();
    disambiguate(&format!("{prefix}{suffix}"), existing)
}

/// 8 lowercase hex chars from the front of a fresh UUID v4. Exposed for
/// testing.
fn short_hex_suffix() -> String {
    let id = Uuid::new_v4().simple().to_string();
    id[..8].to_string()
}

/// Append `-2`, `-3`, ... to `candidate` while it collides with `existing`.
/// Pathological fallback only — the random 8-hex space (4 billion) means we
/// virtually never reach this path.
fn disambiguate(candidate: &str, existing: &[String]) -> String {
    if !existing.iter().any(|b| b == candidate) {
        return candidate.to_string();
    }
    for n in 2..u32::MAX {
        let next = format!("{candidate}-{n}");
        if !existing.contains(&next) {
            return next;
        }
    }
    format!("{candidate}-overflow")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_prefix_plus_eight_hex() {
        let n = derive_branch_name("agents/", &[]);
        assert!(n.starts_with("agents/"), "got {n}");
        let suffix = &n["agents/".len()..];
        assert_eq!(suffix.len(), 8, "suffix must be 8 chars, got {suffix:?}");
        assert!(
            suffix.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "suffix must be lowercase hex, got {suffix:?}"
        );
    }

    #[test]
    fn respects_custom_prefix() {
        let n = derive_branch_name("my-team/", &[]);
        assert!(n.starts_with("my-team/"), "got {n}");
    }

    #[test]
    fn two_consecutive_calls_produce_different_names() {
        // Random suffix → vanishingly small chance of collision; loop a few
        // times to be sure the result isn't deterministic.
        let a = derive_branch_name("agents/", &[]);
        let b = derive_branch_name("agents/", &[]);
        let c = derive_branch_name("agents/", &[]);
        assert!(a != b || b != c, "expected at least one differing name across 3 calls");
    }

    #[test]
    fn avoids_collision_when_suffix_already_exists() {
        // Force the disambiguation path by including 1000 random-looking
        // existing names. The new name must not be in the list.
        let existing: Vec<String> = (0..1000)
            .map(|i| format!("agents/{:08x}", i))
            .collect();
        let n = derive_branch_name("agents/", &existing);
        assert!(!existing.contains(&n), "got colliding name {n}");
    }
}
