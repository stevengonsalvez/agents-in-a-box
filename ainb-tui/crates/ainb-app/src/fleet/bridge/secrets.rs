// ABOUTME: Secret resolver for the native phone-bridge config.
//
// Ported from the Python `ainb_phone_bridge.secrets` (the verified contract):
//   "$ENV_VAR"     -> std::env::var("ENV_VAR").unwrap_or_default()  (warns if unset)
//   "${ENV_VAR}"   -> same branch (strips '$' and surrounding '{}')
//   "keychain:svc" -> `security find-generic-password -s svc -w` (macOS only)
//   "plain-string" -> returned unchanged
//
// A secret is NEVER passed on argv into the daemon — the bridge reads the raw
// reference from config and resolves it here, in-process, at startup. Unknown /
// missing references resolve to "" with a warning rather than erroring, so the
// bridge fails loudly on an empty token at startup (a clearer error than a deep
// stack trace).

use std::process::Command;
use std::time::Duration;

const KEYCHAIN_PREFIX: &str = "keychain:";
/// `security` can hang on a locked-keychain prompt; bound it. (Enforced via a
/// watchdog thread because `std::process` has no built-in timeout.)
const KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Resolve a config secret reference to its plaintext value.
///
/// Pure aside from reading the process environment and (for keychain refs)
/// shelling out to `/usr/bin/security`.
#[must_use]
pub fn resolve_secret(value: &str) -> String {
    resolve_secret_with(value, |name| std::env::var(name).ok())
}

/// Inner resolver with an injectable env lookup, so the `$VAR` / `${VAR}` branch
/// is unit-testable without mutating the process environment (the crate forbids
/// `unsafe`, and `std::env::set_var` is process-global / racy anyway).
fn resolve_secret_with(value: &str, env: impl Fn(&str) -> Option<String>) -> String {
    let raw = value.trim();
    if raw.is_empty() {
        return String::new();
    }

    if let Some(rest) = raw.strip_prefix('$') {
        // Handles both "$VAR" and "${VAR}" — strip the leading '$' and any
        // surrounding braces, then read the environment (default "").
        let name = rest.trim_matches(|c| c == '{' || c == '}');
        let resolved = env(name).unwrap_or_default();
        if resolved.is_empty() {
            tracing::warn!(
                env_var = name,
                "env var referenced by bridge config is unset/empty"
            );
        }
        return resolved;
    }

    if let Some(service) = raw.strip_prefix(KEYCHAIN_PREFIX) {
        if service.is_empty() {
            tracing::warn!("keychain reference has no service name");
            return String::new();
        }
        return resolve_keychain(service);
    }

    // Plain literal — returned as-is.
    raw.to_string()
}

/// Keychain lookup for a `keychain:<service>` reference.
///
/// Two readers, in this order:
///
/// 1. **In-process, via `keyring`.** This is how the settings screen's
///    "store in the keychain" action WRITES the item, and an item is readable
///    without a prompt by the process that created it. Shelling out to
///    `security` for something ainb just wrote fails instead: the ACL trusts
///    the creating binary, not `/usr/bin/security`, so the read raises an
///    authorization dialog and the watchdog below times out before the user can
///    answer it.
/// 2. **`/usr/bin/security`**, for items a user (or an older ainb) put there by
///    hand. Apple-signed and stable, so an "Always Allow" against it sticks
///    across rebuilds.
///
/// Same reference syntax and the same keychain item either way — this is one
/// scheme with two readers, not two schemes.
fn resolve_keychain(service: &str) -> String {
    let owned = service.to_string();
    // BOTH readers are bounded. `keyring::Entry::get_password` blocks on a
    // locked keychain or an ACL prompt exactly like `security` does, so leaving
    // the in-process read unbounded reintroduced the hang the watchdog exists
    // to prevent — and put it on the TUI's startup path.
    if let Some(Some(found)) = bounded(KEYCHAIN_TIMEOUT, move || {
        resolve_keychain_in_process(&owned)
    }) {
        return found;
    }
    resolve_keychain_via_security(service)
}

/// Run `work` on a detached thread and give up after `limit`.
///
/// Detached on purpose: on a timeout we stop waiting, but the thread is NOT
/// joined — a keychain call blocked behind a user dialog would otherwise block
/// us right back. It self-reaps when the call returns, and the send into a
/// dropped channel is a harmless no-op.
fn bounded<T: Send + 'static>(
    limit: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    rx.recv_timeout(limit).ok()
}

/// Step 1: the `keyring` read. `None` means "not found here, try `security`" —
/// including on error, because a keychain ainb cannot open in-process may still
/// be readable by the Apple-signed helper.
fn resolve_keychain_in_process(service: &str) -> Option<String> {
    let entry = keyring::Entry::new(service, crate::credentials::REFERENCE_ACCOUNT).ok()?;
    match entry.get_password() {
        Ok(password) if !password.trim().is_empty() => Some(password.trim().to_string()),
        Ok(_) => None,
        Err(keyring::Error::NoEntry) => None,
        Err(e) => {
            tracing::debug!(service, error = %e, "in-process keychain read failed; trying security");
            None
        }
    }
}

/// Step 2: `/usr/bin/security`, bounded by a watchdog so a locked-keychain
/// prompt can't hang the bridge at startup.
fn resolve_keychain_via_security(service: &str) -> String {
    let service_owned = service.to_string();
    let output = bounded(KEYCHAIN_TIMEOUT, move || {
        Command::new("/usr/bin/security")
            .args(["find-generic-password", "-s", &service_owned, "-w"])
            .output()
    });

    match output {
        Some(Ok(out)) if out.status.success() => {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
        Some(Ok(_)) => {
            tracing::warn!(service, "keychain has no item for service");
            String::new()
        }
        Some(Err(e)) => {
            tracing::warn!(service, error = %e, "keychain lookup failed");
            String::new()
        }
        None => {
            tracing::warn!(service, "keychain lookup timed out");
            String::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The watchdog must give up on a call that never returns. Both keychain
    /// readers block on a locked keychain or an ACL prompt, and the settings
    /// screen resolves secrets on a path a user is waiting on.
    #[test]
    fn a_blocked_lookup_gives_up_instead_of_hanging() {
        let started = std::time::Instant::now();
        let result = bounded(Duration::from_millis(80), || {
            std::thread::sleep(Duration::from_secs(30));
            "never arrives"
        });
        assert_eq!(result, None, "an unbounded lookup would have blocked here");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the watchdog did not fire: waited {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_prompt_lookup_returns_its_value() {
        assert_eq!(bounded(Duration::from_secs(5), || 42), Some(42));
    }

    #[test]
    fn plain_literal_passes_through() {
        assert_eq!(resolve_secret("xoxb-123"), "xoxb-123");
    }

    #[test]
    fn whitespace_only_is_empty() {
        assert_eq!(resolve_secret("   "), "");
        assert_eq!(resolve_secret(""), "");
    }

    #[test]
    fn dollar_env_is_read() {
        // Inject a fake env so the test never mutates the process environment.
        let env = |name: &str| (name == "MYVAR").then(|| "resolved-token".to_string());
        assert_eq!(resolve_secret_with("$MYVAR", &env), "resolved-token");
        assert_eq!(resolve_secret_with("${MYVAR}", &env), "resolved-token");
    }

    #[test]
    fn missing_env_resolves_to_empty() {
        let env = |_: &str| None;
        assert_eq!(
            resolve_secret_with("$AINB_BRIDGE_DEFINITELY_UNSET_XYZ", &env),
            ""
        );
    }

    #[test]
    fn empty_keychain_service_is_empty() {
        assert_eq!(resolve_secret("keychain:"), "");
    }
}
