//! Embedded web dashboard assets. Everything is compiled into the binary;
//! nothing is loaded from external hosts.

use super::text;
use axum::extract::Path;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::Response;
use include_dir::{include_dir, Dir};

static WEB: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/web");

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn file(path: &str, cache: &'static str) -> Response {
    match WEB.get_file(path) {
        Some(f) => {
            let mut r = text(StatusCode::OK, content_type(path), f.contents());
            r.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
            r
        }
        None => text(
            StatusCode::NOT_FOUND,
            "text/plain; charset=utf-8",
            "not found",
        ),
    }
}

/// Dashboard pages. Each has its own path so that pages can be reloaded,
/// bookmarked and linked to; all of them serve the same document.
pub const PAGES: &[&str] = &[
    "overview",
    "send",
    "contacts",
    "inbox",
    "outbox",
    "history",
    "queue",
    "pdu",
    "calls",
    "integrations",
    "modem",
    "system",
    "docs",
];

pub fn is_page(path: &str) -> bool {
    path.strip_prefix('/')
        .map(|p| PAGES.contains(&p))
        .unwrap_or(false)
}

pub async fn index() -> Response {
    file("index.html", "no-cache")
}

pub async fn favicon() -> Response {
    file("favicon.svg", "public, max-age=86400")
}

pub async fn asset(Path(path): Path<String>) -> Response {
    // `include_dir` only resolves embedded entries, so traversal cannot
    // reach the filesystem; reject suspicious paths outright regardless.
    if path.contains("..") || path.starts_with('/') {
        return text(
            StatusCode::NOT_FOUND,
            "text/plain; charset=utf-8",
            "not found",
        );
    }
    file(&format!("assets/{path}"), "no-cache")
}
