//! REST API and web server: routing, access control, request accounting.

mod handlers;
pub mod openapi;
mod web;

use crate::config::Config;
use crate::daemon::Gateway;
use crate::utils;
use axum::body::Body;
use axum::extract::{ConnectInfo, MatchedPath, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct AppState {
    pub gw: Arc<Gateway>,
    limiter: Arc<Mutex<HashMap<IpAddr, (Instant, u32)>>>,
}

/// Per-request context made available to handlers.
#[derive(Clone, Debug)]
pub struct Ctx {
    pub request_id: String,
    pub client_ip: IpAddr,
    /// The caller presented a valid admin-scoped token (config or managed).
    pub admin: bool,
}

/// Uniform JSON error response.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
    pub fn bad_request(m: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", m)
    }
    pub fn not_found(m: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", m)
    }
    pub fn forbidden(m: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", m)
    }
    pub fn conflict(m: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", m)
    }
    pub fn unavailable(m: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "unavailable", m)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = json!({"success": false, "error": {"code": self.code, "message": self.message}});
        (self.status, Json(body)).into_response()
    }
}

impl From<crate::database::DbError> for ApiError {
    fn from(e: crate::database::DbError) -> Self {
        tracing::error!(error = %e, "database error while serving a request");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database_error",
            "the database is unavailable or returned an error",
        )
    }
}

impl From<crate::sms::SubmitError> for ApiError {
    fn from(e: crate::sms::SubmitError) -> Self {
        use crate::sms::SubmitError as E;
        match e {
            E::Invalid(m) => Self::bad_request(m),
            E::Forbidden(m) => Self::forbidden(m),
            E::LimitReached(m) => Self::new(StatusCode::TOO_MANY_REQUESTS, "limit_reached", m),
            E::Unavailable(m) => Self::unavailable(m),
            E::Database(d) => d.into(),
        }
    }
}

pub type ApiResult<T = Json<Value>> = Result<T, ApiError>;

/// JSON body extractor whose rejections use the API error format.
pub struct ApiJson<T>(pub T);

impl<S, T> axum::extract::FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: serde::de::DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        use axum::extract::rejection::JsonRejection as R;
        match Json::<T>::from_request(req, state).await {
            Ok(Json(v)) => Ok(ApiJson(v)),
            Err(R::MissingJsonContentType(_)) => Err(ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_media_type",
                "the request body must be sent as application/json",
            )),
            Err(R::JsonDataError(e)) => Err(ApiError::bad_request(format!(
                "invalid request body: {}",
                e.body_text().trim_start_matches(
                    "Failed to deserialize the JSON body into the target type: "
                )
            ))),
            Err(R::JsonSyntaxError(_)) => {
                Err(ApiError::bad_request("the request body is not valid JSON"))
            }
            Err(e) if e.status() == StatusCode::PAYLOAD_TOO_LARGE => Err(ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "the request body exceeds api.max_request_size_kb",
            )),
            Err(e) => Err(ApiError::new(
                e.status(),
                "invalid_request",
                "the request body could not be read",
            )),
        }
    }
}

/// Query string extractor whose rejections use the API error format.
pub struct ApiQuery<T>(pub T);

impl<S, T> axum::extract::FromRequestParts<S> for ApiQuery<T>
where
    S: Send + Sync,
    T: serde::de::DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        match axum::extract::Query::<T>::from_request_parts(parts, state).await {
            Ok(axum::extract::Query(v)) => Ok(ApiQuery(v)),
            Err(e) => Err(ApiError::bad_request(format!(
                "invalid query string: {}",
                e.body_text()
                    .trim_start_matches("Failed to deserialize query string: ")
            ))),
        }
    }
}

fn ip_in(list: &[String], ip: IpAddr) -> bool {
    // IPv4-mapped IPv6 addresses are compared as IPv4.
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    };
    list.iter().any(|c| match c.parse::<ipnet::IpNet>() {
        Ok(net) => net.contains(&ip),
        Err(_) => c.parse::<IpAddr>().map(|a| a == ip).unwrap_or(false),
    })
}

/// Effective client address. `X-Forwarded-For` is only honoured when the
/// direct peer is a configured trusted proxy; the right-most address that is
/// not itself a trusted proxy is used.
pub fn client_ip(cfg: &Config, peer: IpAddr, headers: &HeaderMap) -> IpAddr {
    if !ip_in(&cfg.api.trusted_proxies, peer) {
        return peer;
    }
    headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|s| s.trim().parse::<IpAddr>().ok())
        .rev()
        .find(|ip| !ip_in(&cfg.api.trusted_proxies, *ip))
        .unwrap_or(peer)
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

fn token_ok(tokens: &[String], headers: &HeaderMap) -> bool {
    match bearer(headers) {
        Some(t) => tokens
            .iter()
            .filter(|x| !x.is_empty())
            .fold(false, |acc, x| acc | utils::secure_eq(x, t)),
        None => false,
    }
}

/// Resolve the presented bearer token to (authenticated, admin). Managed
/// tokens (api_tokens table) are checked after the config tokens; a managed
/// token is matched by the SHA-256 hash of the value and can be revoked
/// instantly, without a reload.
async fn resolve_auth(state: &AppState, cfg: &Config, headers: &HeaderMap) -> (bool, bool) {
    let Some(tok) = bearer(headers) else {
        return (false, false);
    };
    if token_ok(&cfg.api.admin_tokens, headers) {
        return (true, true);
    }
    if token_ok(&cfg.api.tokens, headers) {
        return (true, false);
    }
    let hash = utils::sha256_hex(tok.as_bytes());
    match state.gw.db.api_token_lookup(&hash).await {
        Ok(Some((id, scope))) => {
            let _ = state.gw.db.api_token_touch(&id).await;
            (true, scope == "admin")
        }
        _ => (false, false),
    }
}

fn host_only(h: &str) -> &str {
    if let Some(end) = h.strip_prefix('[').and_then(|r| r.find(']')) {
        return &h[1..=end];
    }
    h.rsplit_once(':').map(|(a, _)| a).unwrap_or(h)
}

/// Access policy for administrative endpoints.
pub fn require_admin(state: &AppState, ctx: &Ctx, headers: &HeaderMap) -> Result<(), ApiError> {
    let cfg = state.gw.cfg.load();
    if !cfg.api.admin_enabled {
        return Err(ApiError::forbidden(
            "administrative endpoints are disabled (api.admin_enabled)",
        ));
    }
    if !cfg.api.admin_allowed_cidrs.is_empty()
        && !ip_in(&cfg.api.admin_allowed_cidrs, ctx.client_ip)
    {
        return Err(ApiError::forbidden(
            "administrative endpoints are not available from this address",
        ));
    }
    // An admin token is required when bearer auth is on, or when admin tokens
    // are configured. A managed admin token satisfies it too (ctx.admin).
    let token_required =
        cfg.api.authentication == "bearer" || !cfg.api.admin_tokens.iter().all(|t| t.is_empty());
    if token_required && !ctx.admin {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "an administrative bearer token is required",
        ));
    }
    let _ = headers;
    Ok(())
}

/// Record an administrative or state-changing action.
pub async fn audit(
    state: &AppState,
    ctx: &Ctx,
    action: &str,
    target: Option<&str>,
    detail: Option<&str>,
) {
    let actor = ctx.client_ip.to_string();
    tracing::info!(
        target: "audit",
        actor = %actor,
        action,
        target = target.unwrap_or("-"),
        detail = detail.unwrap_or("-"),
        request_id = %ctx.request_id,
        "audit"
    );
    if let Err(e) = state
        .gw
        .db
        .audit(Some(&actor), action, target, detail, Some(&ctx.request_id))
        .await
    {
        tracing::error!(error = %e, "could not write audit record");
    }
}

/// Paths that need no bearer token: the dashboard shell and the settings it
/// needs to render its login prompt (title, theme; no secrets).
fn is_public_asset(path: &str) -> bool {
    path == "/"
        || web::is_page(path)
        || path == "/favicon.svg"
        || path == "/branding/logo"
        || path.starts_with("/assets/")
        || path == "/api/v1/ui/config"
}

fn is_probe(path: &str) -> bool {
    path == "/health" || path == "/ready"
}

/// Status webhooks of SMS providers come from the provider's own servers.
/// Their handler authenticates them by signature.
fn is_provider_callback(method: &Method, path: &str) -> bool {
    *method == Method::POST
        && path
            .strip_prefix("/api/v1/providers/")
            .and_then(|rest| rest.strip_suffix("/callback"))
            .map(|name| !name.is_empty() && !name.contains('/'))
            .unwrap_or(false)
}

/// Checks applied to every request before it reaches a handler.
fn authorize(
    state: &AppState,
    cfg: &Config,
    ip: IpAddr,
    req: &Request,
    authenticated: bool,
) -> Result<(), ApiError> {
    let headers = req.headers();
    let path = req.uri().path();
    if is_provider_callback(req.method(), path) {
        return Ok(());
    }
    if !ip_in(&cfg.api.allowed_cidrs, ip) {
        return Err(ApiError::forbidden(
            "access from this address is not allowed (api.allowed_cidrs)",
        ));
    }
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if !cfg.api.allowed_hosts.is_empty()
        && !cfg
            .api
            .allowed_hosts
            .iter()
            .any(|h| h.eq_ignore_ascii_case(host) || h.eq_ignore_ascii_case(host_only(host)))
    {
        return Err(ApiError::new(
            StatusCode::MISDIRECTED_REQUEST,
            "invalid_host",
            "the Host header is not accepted (api.allowed_hosts)",
        ));
    }
    // Cross-site request forgery protection for browser requests that change state.
    if !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        let origin = headers.get(header::ORIGIN).and_then(|h| h.to_str().ok());
        let cors_ok =
            |o: &str| cfg.api.cors_enabled && cfg.api.cors_allowed_origins.iter().any(|a| a == o);
        if let Some(o) = origin {
            let same = o
                .split_once("://")
                .map(|(_, rest)| rest.eq_ignore_ascii_case(host))
                .unwrap_or(false);
            if !same && !cors_ok(o) {
                return Err(ApiError::forbidden("cross-origin request rejected"));
            }
        } else if headers.get("sec-fetch-site").and_then(|h| h.to_str().ok()) == Some("cross-site")
        {
            return Err(ApiError::forbidden("cross-site request rejected"));
        }
    }
    if cfg.api.authentication == "bearer"
        && !authenticated
        && !is_public_asset(path)
        && !is_probe(path)
        && *req.method() != Method::OPTIONS
    {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "a valid bearer token is required",
        ));
    }
    if cfg.api.rate_limit_per_minute > 0 && !is_probe(path) && !is_public_asset(path) {
        let mut map = state.limiter.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() > 10_000 {
            map.retain(|_, (t, _)| t.elapsed() < Duration::from_secs(60));
        }
        let entry = map.entry(ip).or_insert((Instant::now(), 0));
        if entry.0.elapsed() >= Duration::from_secs(60) {
            *entry = (Instant::now(), 0);
        }
        entry.1 += 1;
        if entry.1 > cfg.api.rate_limit_per_minute {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "too many requests; try again shortly",
            ));
        }
    }
    Ok(())
}

async fn guard(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    mut req: Request,
    next: Next,
) -> Response {
    let started = Instant::now();
    let cfg = state.gw.cfg.load_full();
    let ip = client_ip(&cfg, peer.ip(), req.headers());
    // Accept a caller-supplied correlation id when it is well formed.
    let request_id = req
        .headers()
        .get("x-request-id")
        .and_then(|h| h.to_str().ok())
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 64
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        })
        .map(str::to_string)
        .unwrap_or_else(utils::new_id);
    let method = req.method().clone();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| "unmatched".into());
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|h| h.to_str().ok())
        .map(str::to_string);
    let is_asset = is_public_asset(req.uri().path());
    let (authenticated, admin) = resolve_auth(&state, &cfg, req.headers()).await;

    let mut resp = match authorize(&state, &cfg, ip, &req, authenticated) {
        Err(e) => e.into_response(),
        Ok(()) if method == Method::OPTIONS => StatusCode::NO_CONTENT.into_response(),
        Ok(()) => {
            req.extensions_mut().insert(Ctx {
                request_id: request_id.clone(),
                client_ip: ip,
                admin,
            });
            next.run(req).await
        }
    };

    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&request_id) {
        h.insert("x-request-id", v);
    }
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    h.insert(
        "content-security-policy",
        HeaderValue::from_static("default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"),
    );
    if !is_asset {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    if let Some(o) = origin
        .filter(|o| cfg.api.cors_enabled && cfg.api.cors_allowed_origins.iter().any(|a| a == o))
    {
        if let Ok(v) = HeaderValue::from_str(&o) {
            h.insert("access-control-allow-origin", v);
            h.insert("vary", HeaderValue::from_static("Origin"));
            h.insert(
                "access-control-allow-methods",
                HeaderValue::from_static("GET, POST, DELETE, OPTIONS"),
            );
            h.insert(
                "access-control-allow-headers",
                HeaderValue::from_static(
                    "Authorization, Content-Type, Idempotency-Key, X-Request-Id",
                ),
            );
        }
    }
    let status = resp.status().as_u16().to_string();
    let elapsed = started.elapsed().as_secs_f64();
    state.gw.metrics.inc(
        "smsgw_api_requests_total",
        &[
            ("method", method.as_str()),
            ("route", &route),
            ("status", &status),
        ],
    );
    state.gw.metrics.observe(
        "smsgw_api_request_duration_seconds",
        &[("route", &route)],
        elapsed,
    );
    tracing::debug!(request_id = %request_id, method = %method, route = %route, status = %status, duration_ms = (elapsed * 1000.0) as u64, "request");
    resp
}

/// JSON body for requests that fail before reaching a handler.
async fn fallback() -> ApiError {
    ApiError::not_found("no such endpoint")
}

async fn handle_timeout(err: tower::BoxError) -> ApiError {
    if err.is::<tower::timeout::error::Elapsed>() {
        ApiError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "timeout",
            "the request took too long",
        )
    } else {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "internal error",
        )
    }
}

/// Build the application router.
pub fn router(gw: Arc<Gateway>) -> Router {
    use handlers as h;
    let cfg = gw.cfg.load_full();
    let state = AppState {
        gw,
        limiter: Default::default(),
    };
    let v1 = Router::new()
        .route("/status", get(h::status))
        .route("/version", get(h::version))
        .route("/capabilities", get(h::capabilities))
        .route("/stats", get(h::stats))
        .route("/stats/activity", get(h::activity))
        .route("/modem", get(h::modem))
        .route("/modems", get(h::modems))
        .route("/modem/signal", get(h::modem_signal))
        .route("/modem/network", get(h::modem_network))
        .route("/modem/storage", get(h::modem_storage))
        .route("/modem/health", get(h::modem_health))
        .route("/modem/reconnect", post(h::modem_reconnect))
        .route("/modem/at", post(h::modem_at))
        .route("/modem/reset-pin-counter", post(h::reset_pin_counter))
        .route("/sim", get(h::sim))
        .route("/sims", get(h::sims))
        .route("/sms/routes", get(h::sms_routes))
        .route("/sms/send", post(h::sms_send))
        .route("/sms/schedule", post(h::sms_schedule))
        .route("/sms/estimate", post(h::sms_estimate))
        .route("/sms/send-pdu", post(h::sms_send_pdu))
        .route("/messages", get(h::messages))
        .route("/messages/{id}", get(h::message).delete(h::message_delete))
        .route("/messages/{id}/parts", get(h::message_parts))
        .route("/messages/{id}/status", get(h::message_status))
        .route("/queue", get(h::queue))
        .route("/queue/{id}", get(h::queue_item))
        .route("/queue/{id}/retry", post(h::queue_retry))
        .route("/queue/{id}/cancel", post(h::queue_cancel))
        .route("/pdu/encode", post(h::pdu_encode))
        .route("/pdu/decode", post(h::pdu_decode))
        .route("/calls", get(h::calls))
        .route("/calls/{id}", get(h::call))
        .route("/recordings", get(h::recordings))
        .route("/recordings/{id}/download", get(h::recording_download))
        .route("/attachments/{id}/download", get(h::attachment_download))
        .route("/providers", get(h::providers))
        .route("/providers/{name}/callback", post(h::provider_callback))
        .route("/integrations", get(h::integrations))
        .route("/integrations/{name}/status", get(h::integration_status))
        .route("/integrations/{name}/test", post(h::integration_test))
        .route("/security/events", get(h::security_events))
        .route("/security/stats", get(h::security_stats))
        .route("/deliveries", get(h::deliveries))
        .route("/deliveries/{id}/replay", post(h::delivery_replay))
        .route("/dead-letters", get(h::dead_letters))
        .route("/events", get(h::events))
        .route("/events/stream", get(h::events_stream))
        .route("/history/export", get(h::export))
        .route("/ussd", get(h::ussd_history).post(h::ussd))
        .route("/gnss", get(h::gnss))
        .route("/tokens", get(h::tokens).post(h::token_create))
        .route(
            "/tokens/{id}",
            post(h::token_update).delete(h::token_delete),
        )
        .route("/config", get(h::config_view))
        .route("/config/status", get(h::config_status))
        .route("/config/reload", post(h::config_reload))
        .route("/audit", get(h::audit_log))
        .route("/diagnostics", get(h::diagnostics))
        .route("/database/backup", post(h::db_backup))
        .route("/addressbook", get(h::addressbook))
        .route("/contacts", get(h::contacts).post(h::contact_create))
        .route(
            "/contacts/{id}",
            post(h::contact_update).delete(h::contact_delete),
        )
        .route("/ui/config", get(h::ui_config))
        .route("/openapi.json", get(openapi::serve));
    let mut app = Router::new()
        .route("/health", get(h::health))
        .route("/ready", get(h::ready))
        .nest("/api/v1", v1);
    if cfg.metrics.enabled {
        app = app.route(&cfg.metrics.path, get(h::metrics));
    }
    if cfg.web.enabled {
        app = app
            .route("/", get(web::index))
            .route("/favicon.svg", get(web::favicon))
            .route("/branding/logo", get(web::logo))
            .route("/assets/{*path}", get(web::asset));
        for page in web::PAGES {
            app = app.route(&format!("/{page}"), get(web::index));
        }
    }
    app.fallback(fallback)
        .layer(axum::middleware::from_fn_with_state(state.clone(), guard))
        .layer(
            tower::ServiceBuilder::new()
                .layer(axum::error_handling::HandleErrorLayer::new(handle_timeout))
                .layer(tower::timeout::TimeoutLayer::new(Duration::from_secs(
                    cfg.api.request_timeout_seconds,
                ))),
        )
        .layer(tower_http::limit::RequestBodyLimitLayer::new(
            cfg.api.max_request_size_kb * 1024,
        ))
        .with_state(state)
}

/// Bind and serve until the shutdown signal fires.
pub async fn serve(gw: Arc<Gateway>) -> anyhow::Result<()> {
    let cfg = gw.cfg.load_full();
    let addr: SocketAddr = format!(
        "{}:{}",
        if cfg.api.bind.contains(':') {
            format!("[{}]", cfg.api.bind)
        } else {
            cfg.api.bind.clone()
        },
        cfg.api.port
    )
    .parse()?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| anyhow::anyhow!("cannot listen on {addr}: {e}"))?;
    serve_on(gw, listener).await
}

pub async fn serve_on(gw: Arc<Gateway>, listener: tokio::net::TcpListener) -> anyhow::Result<()> {
    let mut shutdown = gw.shutdown_signal();
    tracing::info!(address = %listener.local_addr()?, "HTTP server listening");
    axum::serve(
        listener,
        router(gw).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = shutdown.changed().await;
    })
    .await?;
    Ok(())
}

/// Plain-text response helper.
pub(crate) fn text(
    status: StatusCode,
    content_type: &'static str,
    body: impl Into<Body>,
) -> Response {
    let mut r = Response::new(body.into());
    *r.status_mut() = status;
    r.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    r
}
