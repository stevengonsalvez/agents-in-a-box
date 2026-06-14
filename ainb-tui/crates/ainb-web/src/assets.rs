//! Embedded frontend assets.
//!
//! The vanilla-JS/CSS/HTML frontend under `frontend/` is compiled into the
//! binary with `rust-embed` — no Node build step, no runtime filesystem
//! dependency. Static routes resolve files from here.

use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

/// All files under `crates/ainb-web/frontend/` embedded at build time.
#[derive(RustEmbed)]
#[folder = "frontend/"]
pub struct Frontend;

/// Serve an embedded asset by path, inferring the content type from the
/// extension. Unknown paths fall back to `index.html` so the single-page
/// dashboard works under any sub-path (clean extension point for routing).
pub fn serve(path: &str) -> Response {
    // Map request paths to embedded file names. The frontend references assets
    // under `/static/<name>`; the embed stores them flat at the folder root, so
    // strip the `/static/` prefix. `/` (or empty) resolves to the SPA shell.
    let path = path.trim_start_matches('/');
    let path = path.strip_prefix("static/").unwrap_or(path);
    let path = if path.is_empty() { "index.html" } else { path };

    match Frontend::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (
                [(header::CONTENT_TYPE, mime.as_ref())],
                content.data.into_owned(),
            )
                .into_response()
        }
        None => {
            // SPA fallback: any unknown asset path serves the shell.
            match Frontend::get("index.html") {
                Some(content) => (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    content.data.into_owned(),
                )
                    .into_response(),
                None => (StatusCode::NOT_FOUND, "asset not found").into_response(),
            }
        }
    }
}

/// Axum handler for `GET /` and `GET /static/*path`.
pub async fn handler(uri: Uri) -> Response {
    serve(uri.path())
}
