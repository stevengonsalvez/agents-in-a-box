//! Bearer-token authentication for `/api/*` routes.
//!
//! When the server is configured with a token, every API request must carry
//! `Authorization: Bearer <token>`. Comparison is constant-time to avoid
//! leaking the token through timing. When no token is configured (the default
//! loopback dev posture), all requests are authorized.

use axum::extract::State;
use axum::http::{Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use subtle::ConstantTimeEq;

use crate::routes::AppState;

/// The only route on which the `?token=` query-string fallback is honored.
/// Browsers cannot set an `Authorization` header on an `EventSource`, so the
/// SSE stream accepts the token via query string; every other route requires
/// the header to keep tokens out of logs/history/`Referer`.
const SSE_PATH: &str = "/api/events";

/// Prefix of the WebSocket terminal route. Browsers cannot set an
/// `Authorization` header on a `WebSocket` upgrade either, so — exactly like
/// the SSE stream — the terminal accepts the token via query string. This is
/// the *only* additional path beyond [`SSE_PATH`] that honors the query-token
/// fallback; the exposure is deliberately kept to the two streaming surfaces
/// that physically cannot send a header. Every JSON `/api/*` route still
/// requires the bearer header so tokens never leak via logs/history/`Referer`.
const WS_PREFIX: &str = "/ws/session/";

/// True when `path` is one of the two streaming surfaces (SSE or the WS
/// terminal) on which the `?token=` query-string fallback is honored.
fn query_token_allowed(path: &str) -> bool {
    path == SSE_PATH || path.starts_with(WS_PREFIX)
}

/// JSON 401 body matching the API error envelope.
fn unauthorized() -> Response {
    let body = axum::Json(serde_json::json!({
        "error": { "code": "UNAUTHORIZED", "message": "missing or invalid bearer token" }
    }));
    (StatusCode::UNAUTHORIZED, body).into_response()
}

/// Extract the `Bearer` value from an `Authorization` header, if present.
fn bearer_value(req_headers: &header::HeaderMap) -> Option<&str> {
    req_headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

/// Extract a `token` query-string parameter, if present. Browsers cannot set
/// headers on an `EventSource` connection, so the SSE endpoint additionally
/// accepts the token via query string (the client strips it from the URL after
/// connecting). This mirrors agent-deck's `authorizeWSRequest`.
fn query_token(query: Option<&str>) -> Option<String> {
    let query = query?;
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        if k == "token" {
            Some(urldecode(v))
        } else {
            None
        }
    })
}

/// Minimal percent-decoder for query-string token values (handles `%XX` and
/// `+`). Avoids pulling in a URL crate for one field.
fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push((h * 16 + l) as u8);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// True when `presented` matches `expected` in constant time.
fn token_matches(expected: &str, presented: &str) -> bool {
    // ConstantTimeEq over equal-length byte slices; lengths differing is an
    // immediate non-match but we still feed equal-length work for the common
    // case by comparing bytes directly via subtle.
    let exp = expected.as_bytes();
    let got = presented.as_bytes();
    if exp.len() != got.len() {
        return false;
    }
    exp.ct_eq(got).into()
}

/// Axum middleware enforcing bearer auth on `/api/*` when a token is set.
pub async fn require_bearer(
    State(state): State<AppState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let Some(expected) = state.config.token.as_deref() else {
        // No token configured — loopback dev mode, all requests allowed.
        return next.run(req).await;
    };

    // Header is the primary path, honored on every route. The `?token=` query
    // fallback is scoped to the two streaming surfaces that physically cannot
    // send an `Authorization` header — the SSE `EventSource` and the WebSocket
    // terminal upgrade (see [`query_token_allowed`]). Tokens in URLs leak via
    // proxy/access logs, browser history, and `Referer`, so we must not accept
    // them on any other route — a header is required everywhere else.
    let header_ok = bearer_value(req.headers()).is_some_and(|t| token_matches(expected, t));
    let query_ok = query_token_allowed(req.uri().path())
        && query_token(req.uri().query())
            .as_deref()
            .is_some_and(|t| token_matches(expected, t));

    if header_ok || query_ok {
        next.run(req).await
    } else {
        unauthorized()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_token_accepted() {
        assert!(token_matches("s3cret", "s3cret"));
    }

    #[test]
    fn wrong_token_rejected() {
        assert!(!token_matches("s3cret", "nope"));
    }

    #[test]
    fn length_mismatch_rejected() {
        assert!(!token_matches("s3cret", "s3cre"));
        assert!(!token_matches("s3cret", "s3cretx"));
    }

    #[test]
    fn bearer_value_parsing() {
        let mut h = header::HeaderMap::new();
        h.insert(header::AUTHORIZATION, "Bearer abc123".parse().unwrap());
        assert_eq!(bearer_value(&h), Some("abc123"));

        let mut h2 = header::HeaderMap::new();
        h2.insert(header::AUTHORIZATION, "Basic abc123".parse().unwrap());
        assert_eq!(bearer_value(&h2), None);

        assert_eq!(bearer_value(&header::HeaderMap::new()), None);
    }

    #[test]
    fn query_token_allowed_only_on_streaming_surfaces() {
        // The two surfaces that cannot send an Authorization header.
        assert!(query_token_allowed("/api/events"));
        assert!(query_token_allowed("/ws/session/abc-123"));
        assert!(query_token_allowed("/ws/session/"));
        // Every JSON route still requires the header — query token refused.
        assert!(!query_token_allowed("/api/snapshot"));
        assert!(!query_token_allowed("/api/sessions"));
        assert!(!query_token_allowed("/api/push/subscribe"));
        assert!(!query_token_allowed("/healthz"));
        // Guard against a sneaky prefix that merely contains the WS segment.
        assert!(!query_token_allowed("/api/ws/session/abc"));
    }

    #[test]
    fn query_token_extraction() {
        assert_eq!(query_token(Some("token=abc123")), Some("abc123".into()));
        assert_eq!(
            query_token(Some("foo=1&token=s3%20cret&bar=2")),
            Some("s3 cret".into())
        );
        assert_eq!(query_token(Some("foo=1")), None);
        assert_eq!(query_token(None), None);
    }
}
