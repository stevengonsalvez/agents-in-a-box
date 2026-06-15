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

/// macOS keychain lookup, bounded by a watchdog so a locked-keychain prompt
/// can't hang the bridge at startup.
fn resolve_keychain(service: &str) -> String {
    let service_owned = service.to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let out = Command::new("/usr/bin/security")
            .args(["find-generic-password", "-s", &service_owned, "-w"])
            .output();
        let _ = tx.send(out);
    });

    match rx.recv_timeout(KEYCHAIN_TIMEOUT) {
        Ok(Ok(out)) if out.status.success() => {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
        Ok(Ok(_)) => {
            tracing::warn!(service, "keychain has no item for service");
            String::new()
        }
        Ok(Err(e)) => {
            tracing::warn!(service, error = %e, "keychain lookup failed");
            String::new()
        }
        Err(_) => {
            tracing::warn!(service, "keychain lookup timed out");
            String::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
