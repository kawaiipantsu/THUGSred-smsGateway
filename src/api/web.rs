//! Embedded web dashboard assets. Everything is compiled into the binary;
//! nothing is loaded from external hosts.

use super::{text, AppState};
use axum::extract::{Path, State};
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

fn escape(v: &str) -> String {
    v.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Largest logo file that is served.
const MAX_LOGO_BYTES: u64 = 2 * 1024 * 1024;

/// Address of the configured logo, or `None` when the built-in one is used.
/// The modification time makes the address change when the file does.
pub fn logo_url(cfg: &crate::config::Config) -> Option<String> {
    let path = &cfg.branding.logo;
    crate::config::logo_media_type(path)?;
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_LOGO_BYTES {
        return None;
    }
    let stamp = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Some(format!("/branding/logo?v={stamp}"))
}

/// The application shell, with the configured name and theme filled in so
/// that the page looks right before its script has run.
pub async fn index(State(s): State<AppState>) -> Response {
    let Some(f) = WEB.get_file("index.html").and_then(|f| f.contents_utf8()) else {
        return file("index.html", "no-cache");
    };
    let cfg = s.gw.cfg.load();
    let theme = match cfg.brand_theme() {
        "light" => "light",
        _ => "dark",
    };
    let custom = logo_url(&cfg);
    let nav_logo = if custom.is_some() { "" } else { "hidden" };
    let icon = custom.unwrap_or_else(|| "/favicon.svg".into());
    let html = f
        .replace("{{title}}", &escape(cfg.brand_title()))
        .replace("{{subtitle}}", &escape(&cfg.branding.subtitle))
        .replace("{{theme}}", theme)
        .replace("{{nav_logo}}", nav_logo)
        .replace("{{icon}}", &escape(&icon));
    let mut r = text(StatusCode::OK, "text/html; charset=utf-8", html);
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    r
}

/// The custom logo from branding.conf.
pub async fn logo(State(s): State<AppState>) -> Response {
    let missing = || {
        text(
            StatusCode::NOT_FOUND,
            "text/plain; charset=utf-8",
            "not found",
        )
    };
    let cfg = s.gw.cfg.load_full();
    if logo_url(&cfg).is_none() {
        return missing();
    }
    let Some(kind) = crate::config::logo_media_type(&cfg.branding.logo) else {
        return missing();
    };
    match tokio::fs::read(&cfg.branding.logo).await {
        Ok(bytes) if bytes.len() as u64 <= MAX_LOGO_BYTES => {
            let mut r = text(StatusCode::OK, kind, bytes);
            r.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=86400"),
            );
            r
        }
        _ => missing(),
    }
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
