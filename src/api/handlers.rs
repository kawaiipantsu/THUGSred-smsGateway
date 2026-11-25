//! Request handlers for the REST API.

use super::{audit, require_admin, text, ApiError, ApiJson, ApiQuery, ApiResult, AppState, Ctx};
use crate::daemon::{self, version_info};
use crate::database::{Message, MessageQuery};
use crate::modem::{CallSignal, CallSignalKind, Modem};
use crate::sms::pdu::{self, Encoding, Payload, PduDirection, SubmitRequest};
use crate::sms::SendRequest;
use crate::utils;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use chrono::{DateTime, Utc};
use futures::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

type S = State<AppState>;
type C = Extension<Ctx>;

fn ok(data: Value) -> ApiResult {
    Ok(Json(json!({"success": true, "data": data})))
}

fn paged(data: Value, total: i64, p: &Page) -> ApiResult {
    Ok(Json(
        json!({"success": true, "data": data, "pagination": {"total": total, "limit": p.limit(), "offset": p.offset()}}),
    ))
}

#[derive(Deserialize, Default)]
pub struct Page {
    limit: Option<i64>,
    offset: Option<i64>,
}

impl Page {
    fn limit(&self) -> i64 {
        self.limit.unwrap_or(50).clamp(1, 500)
    }
    fn offset(&self) -> i64 {
        self.offset.unwrap_or(0).max(0)
    }
}

#[derive(Deserialize, Default)]
pub struct ModemSel {
    modem: Option<String>,
}

fn pick<'a>(state: &'a AppState, sel: &ModemSel) -> Result<&'a Modem, ApiError> {
    state
        .gw
        .modem(sel.modem.as_deref())
        .ok_or_else(|| ApiError::not_found("no such modem"))
}

/// Parse a date filter: RFC 3339 or a plain `YYYY-MM-DD` date.
fn parse_time(
    name: &str,
    v: &Option<String>,
    end_of_day: bool,
) -> Result<Option<String>, ApiError> {
    let Some(s) = v.as_deref().filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Ok(Some(utils::ts(d.with_timezone(&Utc))));
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let t = if end_of_day {
            d.and_hms_milli_opt(23, 59, 59, 999)
        } else {
            d.and_hms_opt(0, 0, 0)
        };
        if let Some(t) = t {
            return Ok(Some(utils::ts(t.and_utc())));
        }
    }
    Err(ApiError::bad_request(format!(
        "{name}: expected an RFC 3339 timestamp or YYYY-MM-DD date"
    )))
}

// ---- health, status, metrics ----------------------------------------------

pub async fn health(State(s): S) -> Json<Value> {
    Json(
        json!({"status": "ok", "version": daemon::VERSION, "uptime_seconds": s.gw.metrics.uptime_seconds()}),
    )
}

/// Ready when the database answers. Modem availability is reported but does
/// not make the daemon unready: history and the API keep working without it.
pub async fn ready(State(s): S) -> Response {
    let db = s.gw.db.ping().await;
    let modems: Vec<Value> =
        s.gw.modems
            .iter()
            .map(|m| json!({"id": m.id, "state": m.status().state}))
            .collect();
    let body = Json(json!({"ready": db, "database": db, "modems": modems}));
    (
        if db {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        body,
    )
        .into_response()
}

pub async fn metrics(State(s): S) -> Response {
    text(
        StatusCode::OK,
        "text/plain; version=0.0.4; charset=utf-8",
        s.gw.metrics.render(),
    )
}

pub async fn version() -> ApiResult {
    ok(version_info())
}

fn modem_summary(state: &AppState, m: &Modem) -> Value {
    let st = m.status();
    let cfg = state.gw.cfg.load();
    let sim = st
        .sim
        .profile
        .as_deref()
        .and_then(|id| cfg.sim(id))
        .or_else(|| cfg.sim_for_modem(&m.id));
    json!({
        "id": st.id,
        "state": st.state,
        "enabled": st.enabled,
        "manufacturer": st.manufacturer,
        "model": st.model,
        "operator": st.network.operator,
        "registration": st.network.registration,
        "signal_percent": st.signal.percent,
        "rssi_dbm": st.signal.rssi_dbm,
        "sim_state": st.sim.state,
        "sim_id": sim.map(|s| s.id.clone()),
        "mobile_number": sim.map(|s| s.mobile_number.clone()).filter(|n| !n.is_empty()),
        "last_error": st.last_error,
        "hint": st.hint,
    })
}

pub async fn status(State(s): S) -> ApiResult {
    let cfg = s.gw.cfg.load_full();
    let db_ok = s.gw.db.ping().await;
    let mut integrations = Vec::new();
    for i in &cfg.integration {
        let stats =
            s.gw.db
                .integration_stats(&i.name)
                .await
                .unwrap_or(Value::Null);
        let healthy = !i.enabled
            || stats["last_attempt"]["success"]
                .as_i64()
                .map(|v| v == 1)
                .unwrap_or(true);
        integrations.push(json!({"name": i.name, "type": i.kind, "enabled": i.enabled, "healthy": healthy, "dead": stats["dead"], "pending": stats["pending"]}));
    }
    let disk = crate::metrics::disk_usage(&cfg.paths.data)
        .map(|(free, total)| json!({"free_bytes": free, "total_bytes": total}));
    ok(json!({
        "daemon": {
            "status": "running",
            "instance_id": cfg.general.instance_id,
            "instance_name": cfg.general.instance_name,
            "version": daemon::VERSION,
            "commit": daemon::GIT_COMMIT,
            "started_at": s.gw.started_at,
            "uptime_seconds": s.gw.metrics.uptime_seconds(),
        },
        "modems": s.gw.modems.iter().map(|m| modem_summary(&s, m)).collect::<Vec<_>>(),
        "database": {
            "healthy": db_ok,
            "driver": cfg.database.driver,
            "schema_version": s.gw.db.schema_version().await.unwrap_or(0),
            "size_bytes": s.gw.db.size_bytes(),
        },
        "queue": {"pending": s.gw.db.queue_depth().await.unwrap_or(-1)},
        "integrations": integrations,
        "disk": disk,
        "config": s.gw.config_status(),
    }))
}

pub async fn capabilities(State(s): S) -> ApiResult {
    let cfg = s.gw.cfg.load_full();
    let modems: Vec<Value> = s
        .gw
        .modems
        .iter()
        .map(|m| {
            let st = m.status();
            json!({"id": st.id, "state": st.state, "manufacturer": st.manufacturer, "model": st.model, "firmware": st.firmware, "profile": st.profile, "device": st.device, "baud_rate": st.baud_rate, "capabilities": st.capabilities})
        })
        .collect();
    ok(json!({
        "gateway": {
            "sms_receive": cfg.sms.enabled && cfg.sms.receive_enabled,
            "sms_send": cfg.sms.enabled && cfg.sms.send_enabled,
            "encodings": ["gsm7", "ucs2", "8bit"],
            "multipart": cfg.sms.allow_multipart,
            "max_message_parts": cfg.sms.max_message_parts,
            "flash_sms": cfg.sms.allow_flash_sms,
            "binary_sms": cfg.sms.allow_binary,
            "delivery_reports": true,
            "scheduled_sms": true,
            "mms_notifications": true,
            "mms_retrieval": if cfg.mms.enabled && cfg.mms.download_attachments { "experimental" } else { "disabled" },
            "mms_sending": "unsupported",
            "ussd": cfg.ussd.enabled,
            "calls": cfg.calls.enabled,
            "call_auto_answer": cfg.calls.auto_answer,
            "call_recording": cfg.calls.recording_enabled,
            "gnss": cfg.gnss.enabled,
            "raw_at_commands": cfg.security.allow_raw_at_commands,
            "raw_pdu_submission": cfg.security.allow_raw_pdu_submission,
            "database": cfg.database.driver,
            "integration_types": crate::config::INTEGRATION_TYPES,
            "events": crate::config::KNOWN_EVENTS,
            "modem_profiles": s.gw.profiles.names(),
        },
        "modems": modems,
    }))
}

pub async fn stats(State(s): S) -> ApiResult {
    let mut v = s.gw.db.stats().await?;
    v["tables"] = s.gw.db.table_counts().await?;
    v["uptime_seconds"] = json!(s.gw.metrics.uptime_seconds());
    ok(v)
}

#[derive(Deserialize)]
pub struct ActivityQ {
    range: Option<String>,
}

pub async fn activity(State(s): S, ApiQuery(q): ApiQuery<ActivityQ>) -> ApiResult {
    let range = q.range.as_deref().unwrap_or("24h");
    let (hours, bucket_len) = match range {
        "24h" => (24, 13),
        "7d" => (7 * 24, 10),
        "30d" => (30 * 24, 10),
        _ => return Err(ApiError::bad_request("range must be one of 24h, 7d, 30d")),
    };
    let since = Utc::now() - chrono::Duration::hours(hours);
    let rows = s.gw.db.activity(&utils::ts(since), bucket_len).await?;
    // Emit every bucket, including empty ones, so clients can draw directly.
    let mut buckets = Vec::new();
    let step = if bucket_len == 13 {
        chrono::Duration::hours(1)
    } else {
        chrono::Duration::days(1)
    };
    let fmt = if bucket_len == 13 {
        "%Y-%m-%dT%H"
    } else {
        "%Y-%m-%d"
    };
    let mut t = since;
    let mut seen = std::collections::HashSet::new();
    while t <= Utc::now() + step {
        let key = t.format(fmt).to_string();
        if key > Utc::now().format(fmt).to_string() {
            break;
        }
        if seen.insert(key.clone()) {
            let count = |dir: &str| {
                rows.iter()
                    .find(|r| r["bucket"] == key.as_str() && r["direction"] == dir)
                    .and_then(|r| r["count"].as_i64())
                    .unwrap_or(0)
            };
            buckets.push(
                json!({"bucket": key, "inbound": count("inbound"), "outbound": count("outbound")}),
            );
        }
        t += step;
    }
    ok(
        json!({"range": range, "bucket": if bucket_len == 13 { "hour" } else { "day" }, "timezone": "UTC", "buckets": buckets}),
    )
}

// ---- modem and SIM ----------------------------------------------------------

pub async fn modems(State(s): S) -> ApiResult {
    ok(json!(s
        .gw
        .modems
        .iter()
        .map(|m| m.status())
        .collect::<Vec<_>>()))
}

pub async fn modem(State(s): S, ApiQuery(sel): ApiQuery<ModemSel>) -> ApiResult {
    ok(json!(pick(&s, &sel)?.status()))
}

pub async fn modem_signal(State(s): S, ApiQuery(sel): ApiQuery<ModemSel>) -> ApiResult {
    let st = pick(&s, &sel)?.status();
    ok(
        json!({"modem": st.id, "state": st.state, "signal": st.signal, "updated_at": st.last_seen_at}),
    )
}

pub async fn modem_network(State(s): S, ApiQuery(sel): ApiQuery<ModemSel>) -> ApiResult {
    let st = pick(&s, &sel)?.status();
    ok(
        json!({"modem": st.id, "state": st.state, "network": st.network, "technologies": st.capabilities.network_technologies}),
    )
}

pub async fn modem_storage(State(s): S, ApiQuery(sel): ApiQuery<ModemSel>) -> ApiResult {
    let st = pick(&s, &sel)?.status();
    ok(
        json!({"modem": st.id, "state": st.state, "storage": st.storage, "available_storages": st.capabilities.sms_storages}),
    )
}

pub async fn modem_health(
    State(s): S,
    ApiQuery(sel): ApiQuery<ModemSel>,
    ApiQuery(p): ApiQuery<Page>,
) -> ApiResult {
    let m = pick(&s, &sel)?;
    ok(json!(s.gw.db.health_history(&m.id, p.limit()).await?))
}

pub async fn modem_reconnect(
    State(s): S,
    Extension(ctx): C,
    headers: HeaderMap,
    ApiQuery(sel): ApiQuery<ModemSel>,
) -> ApiResult {
    require_admin(&s, &ctx, &headers)?;
    let m = pick(&s, &sel)?;
    audit(&s, &ctx, "modem.reconnect", Some(&m.id), None).await;
    if !m.reconnect().await {
        return Err(ApiError::unavailable("modem task is not running"));
    }
    Ok(Json(
        json!({"success": true, "data": {"modem": m.id, "reconnecting": true}}),
    ))
}

#[derive(Deserialize)]
pub struct AtBody {
    command: String,
    modem: Option<String>,
}

pub async fn modem_at(
    State(s): S,
    Extension(ctx): C,
    headers: HeaderMap,
    ApiJson(b): ApiJson<AtBody>,
) -> ApiResult {
    require_admin(&s, &ctx, &headers)?;
    let cfg = s.gw.cfg.load_full();
    if !cfg.security.allow_raw_at_commands {
        return Err(ApiError::forbidden(
            "raw AT commands are disabled (security.allow_raw_at_commands)",
        ));
    }
    let cmd = b.command.trim().to_string();
    let upper = cmd.to_ascii_uppercase();
    if !upper.starts_with("AT") || cmd.len() > 200 || cmd.chars().any(|c| c.is_control()) {
        return Err(ApiError::bad_request(
            "command must be a single AT command without control characters",
        ));
    }
    // Commands that could lock or block the SIM, or submit unlogged messages,
    // are never accepted through this endpoint.
    for blocked in [
        "AT+CPIN=",
        "AT+CLCK",
        "AT+CPWD",
        "AT+CMGS",
        "AT+CMGW",
        "AT+CMSS",
        "AT+CFUN=0",
        "AT+CPOWD",
        "AT&F",
        "ATZ",
        "AT+IPR=",
    ] {
        if upper.starts_with(blocked) {
            return Err(ApiError::forbidden(format!(
                "{blocked} is not permitted through the raw AT endpoint"
            )));
        }
    }
    let m =
        s.gw.modem(b.modem.as_deref())
            .ok_or_else(|| ApiError::not_found("no such modem"))?;
    audit(&s, &ctx, "modem.raw_at", Some(&m.id), Some(&cmd)).await;
    if let Some(number) = upper.strip_prefix("ATD") {
        let number: String = number
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '+')
            .collect();
        let _ = s.gw.calls_tx.try_send(CallSignal {
            modem_id: m.id.clone(),
            kind: CallSignalKind::Dialed(number),
        });
    }
    match m.raw_at(&cmd).await {
        Ok(lines) => ok(json!({"modem": m.id, "command": cmd, "ok": true, "response": lines})),
        Err(e) => ok(json!({"modem": m.id, "command": cmd, "ok": false, "error": e})),
    }
}

pub async fn reset_pin_counter(
    State(s): S,
    Extension(ctx): C,
    headers: HeaderMap,
    ApiQuery(sel): ApiQuery<ModemSel>,
) -> ApiResult {
    require_admin(&s, &ctx, &headers)?;
    let m = pick(&s, &sel)?;
    let iccid = m.status().sim.iccid.unwrap_or_else(|| "-".into());
    s.gw.db
        .kv_set(&format!("pin_attempts:{}:{}", m.id, iccid), "0")
        .await?;
    audit(&s, &ctx, "sim.reset_pin_counter", Some(&m.id), None).await;
    ok(
        json!({"modem": m.id, "pin_attempts": 0, "note": "the PIN is tried again at the next SIM check; make sure sim.conf holds the correct PIN"}),
    )
}

fn sim_view(state: &AppState, m: &Modem) -> Value {
    let st = m.status();
    let cfg = state.gw.cfg.load();
    let sim = st
        .sim
        .profile
        .as_deref()
        .and_then(|id| cfg.sim(id))
        .or_else(|| cfg.sim_for_modem(&m.id));
    // The PIN and APN password never leave the daemon.
    let configured = sim.map(|p| {
        json!({
            "id": p.id, "name": p.name, "owner": p.owner, "description": p.description,
            "mobile_number": p.mobile_number, "country_code": p.country_code, "carrier": p.carrier,
            "pin_enabled": p.pin_enabled, "smsc": p.smsc,
            "network": {"selection": p.network.selection, "operator_code": p.network.operator_code, "roaming_enabled": p.network.roaming_enabled, "apn": p.network.apn},
            "identity": p.identity, "limits": p.limits,
        })
    });
    json!({
        "modem": st.id,
        "state": st.sim.state,
        "ready": st.sim.state == "ready",
        "detected": {"iccid": st.sim.iccid, "imsi": st.sim.imsi, "msisdn": st.sim.msisdn, "smsc": st.sim.smsc, "operator": st.network.operator},
        "configured": configured,
        "note": "mobile_number is descriptive gateway metadata; the network determines the number recipients see",
    })
}

pub async fn sim(State(s): S, ApiQuery(sel): ApiQuery<ModemSel>) -> ApiResult {
    ok(sim_view(&s, pick(&s, &sel)?))
}

pub async fn sims(State(s): S) -> ApiResult {
    ok(json!(s
        .gw
        .modems
        .iter()
        .map(|m| sim_view(&s, m))
        .collect::<Vec<_>>()))
}

// ---- sending ----------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SendBody {
    to: Option<String>,
    message: Option<String>,
    /// Hexadecimal binary payload (8-bit SMS).
    binary: Option<String>,
    #[serde(default)]
    encoding: Encoding,
    delivery_report: Option<bool>,
    priority: Option<Value>,
    #[serde(default)]
    flash: bool,
    message_class: Option<u8>,
    validity_minutes: Option<u32>,
    scheduled_at: Option<String>,
    modem: Option<String>,
    from: Option<String>,
    provider: Option<String>,
    sender_name: Option<String>,
    destination_port: Option<u16>,
    source_port: Option<u16>,
    protocol_identifier: Option<u8>,
    correlation_id: Option<String>,
    #[serde(default)]
    override_allowlist: bool,
}

fn to_request(b: SendBody, origin: &str) -> Result<SendRequest, ApiError> {
    let priority = match &b.priority {
        None => 0,
        Some(Value::String(s)) => match s.as_str() {
            "high" => 10,
            "normal" => 0,
            "low" => -10,
            _ => {
                return Err(ApiError::bad_request(
                    "priority must be high, normal, low or an integer from -100 to 100",
                ))
            }
        },
        Some(Value::Number(n)) => n
            .as_i64()
            .filter(|v| (-100..=100).contains(v))
            .ok_or_else(|| ApiError::bad_request("priority must be between -100 and 100"))?,
        Some(_) => {
            return Err(ApiError::bad_request(
                "priority must be high, normal, low or an integer",
            ))
        }
    };
    let binary = match b.binary.as_deref() {
        Some(h) => Some(
            hex::decode(h.trim())
                .map_err(|_| ApiError::bad_request("binary must be hexadecimal"))?,
        ),
        None => None,
    };
    let scheduled_at = match b.scheduled_at.as_deref() {
        Some(s) => Some(
            DateTime::parse_from_rfc3339(s)
                .map_err(|_| ApiError::bad_request("scheduled_at must be an RFC 3339 timestamp"))?
                .with_timezone(&Utc),
        ),
        None => None,
    };
    if b.correlation_id
        .as_deref()
        .map(|c| c.len() > 128)
        .unwrap_or(false)
    {
        return Err(ApiError::bad_request(
            "correlation_id is limited to 128 characters",
        ));
    }
    Ok(SendRequest {
        to: b.to.unwrap_or_default(),
        message: b.message,
        binary,
        encoding: b.encoding,
        delivery_report: b.delivery_report,
        priority,
        flash: b.flash,
        message_class: b.message_class,
        validity_minutes: b.validity_minutes,
        scheduled_at,
        modem: b.modem,
        from: b.from,
        provider: b.provider,
        sender_name: b.sender_name,
        destination_port: b.destination_port,
        source_port: b.source_port,
        protocol_identifier: b.protocol_identifier,
        idempotency_key: None,
        correlation_id: b.correlation_id,
        override_allowlist: b.override_allowlist,
        origin: origin.to_string(),
    })
}

async fn do_send(
    s: &AppState,
    ctx: &Ctx,
    headers: &HeaderMap,
    body: SendBody,
    require_schedule: bool,
) -> Result<Response, ApiError> {
    let mut req = to_request(body, "api")?;
    if require_schedule && req.scheduled_at.is_none() {
        return Err(ApiError::bad_request("scheduled_at is required"));
    }
    if req.override_allowlist {
        require_admin(s, ctx, headers)?;
    }
    if req.protocol_identifier.unwrap_or(0) != 0
        && !s.gw.cfg.load().security.allow_raw_pdu_submission
    {
        return Err(ApiError::forbidden(
            "a custom protocol_identifier requires security.allow_raw_pdu_submission",
        ));
    }
    req.idempotency_key = headers
        .get("idempotency-key")
        .and_then(|h| h.to_str().ok())
        .map(str::to_string);
    if req.correlation_id.is_none() {
        req.correlation_id = Some(ctx.request_id.clone());
    }
    let done = s.gw.sms.submit(req).await?;
    let m = &done.message;
    if !done.duplicate {
        audit(
            s,
            ctx,
            "sms.submit",
            Some(&m.id),
            Some(&format!(
                "parts={} encoding={}",
                m.parts,
                m.encoding.as_deref().unwrap_or("")
            )),
        )
        .await;
    }
    let body = json!({
        "success": true,
        "message_id": m.id,
        "status": m.status,
        "encoding": m.encoding,
        "parts": m.parts,
        "duplicate": done.duplicate,
        "modem": m.modem_id,
        "provider": m.modem_id.as_deref().and_then(crate::providers::provider_of),
        "sender": m.sender,
    });
    Ok((StatusCode::ACCEPTED, Json(body)).into_response())
}

pub async fn sms_send(
    State(s): S,
    Extension(ctx): C,
    headers: HeaderMap,
    ApiJson(b): ApiJson<SendBody>,
) -> Result<Response, ApiError> {
    do_send(&s, &ctx, &headers, b, false).await
}

pub async fn sms_schedule(
    State(s): S,
    Extension(ctx): C,
    headers: HeaderMap,
    ApiJson(b): ApiJson<SendBody>,
) -> Result<Response, ApiError> {
    do_send(&s, &ctx, &headers, b, true).await
}

pub async fn sms_estimate(State(s): S, ApiJson(b): ApiJson<SendBody>) -> ApiResult {
    let req = to_request(
        SendBody {
            to: Some("+0".into()),
            ..b
        },
        "api",
    )?;
    let e = s.gw.sms.estimate(&req)?;
    let over = e.parts > s.gw.cfg.load().sms.max_message_parts;
    ok(
        json!({"encoding": e.encoding, "parts": e.parts, "characters": e.characters, "units": e.units, "units_per_part": e.units_per_part, "units_remaining": e.units_remaining, "exceeds_limit": over}),
    )
}

#[derive(Deserialize)]
pub struct RawPduBody {
    pdu: String,
    modem: Option<String>,
}

/// Submit a caller-built SMS-SUBMIT PDU. Disabled unless explicitly enabled.
pub async fn sms_send_pdu(
    State(s): S,
    Extension(ctx): C,
    headers: HeaderMap,
    ApiJson(b): ApiJson<RawPduBody>,
) -> ApiResult {
    require_admin(&s, &ctx, &headers)?;
    let cfg = s.gw.cfg.load_full();
    if !cfg.security.allow_raw_pdu_submission {
        return Err(ApiError::forbidden(
            "raw PDU submission is disabled (security.allow_raw_pdu_submission)",
        ));
    }
    let pdu_hex = b.pdu.trim().to_ascii_uppercase();
    let pdu::Pdu::Submit(sub) = pdu::decode(&pdu_hex, PduDirection::MobileOriginated, true)
        .map_err(|e| {
            ApiError::bad_request(format!("not a valid SMS-SUBMIT PDU in modem format: {e}"))
        })?
    else {
        return Err(ApiError::bad_request("PDU is not an SMS-SUBMIT"));
    };
    let to = sub.recipient.number.clone();
    if cfg
        .security
        .destination_blocklist
        .iter()
        .any(|p| utils::list_matches(p, &to))
        || (!cfg.security.destination_allowlist.is_empty()
            && !cfg
                .security
                .destination_allowlist
                .iter()
                .any(|p| utils::list_matches(p, &to)))
    {
        return Err(ApiError::forbidden(
            "destination is not permitted by the destination allowlist/blocklist",
        ));
    }
    let m =
        s.gw.modem(b.modem.as_deref())
            .ok_or_else(|| ApiError::not_found("no such modem"))?;
    let bytes =
        hex::decode(&pdu_hex).map_err(|_| ApiError::bad_request("PDU must be hexadecimal"))?;
    let smsc_len = bytes[0] as usize + 1;
    let tpdu_len = bytes.len() - smsc_len;
    let now = utils::now();
    let id = utils::new_id();
    let mut msg = Message {
        id: id.clone(),
        direction: "outbound".into(),
        message_type: "raw_pdu".into(),
        instance_id: cfg.general.instance_id.clone(),
        modem_id: Some(m.id.clone()),
        recipient: Some(to.clone()),
        normalized_number: Some(utils::normalize_number(&to)),
        body: sub.user_data.text.clone(),
        raw_pdu: Some(pdu_hex.clone()),
        encoding: Some(sub.user_data.encoding.as_str().into()),
        protocol_id: Some(sub.protocol_identifier as i64),
        data_coding: Some(sub.data_coding_scheme.raw as i64),
        parts: 1,
        status: "processing".into(),
        delivery_report: sub.status_report_request,
        correlation_id: Some(ctx.request_id.clone()),
        metadata: Some(json!({"origin": "api:raw_pdu"})),
        created_at: now.clone(),
        updated_at: now.clone(),
        ..Default::default()
    };
    let part = crate::database::Part {
        id: utils::new_id(),
        message_id: Some(id.clone()),
        direction: "outbound".into(),
        modem_id: Some(m.id.clone()),
        address: Some(to),
        sequence: 1,
        pdu: Some(pdu_hex.clone()),
        tpdu_length: Some(tpdu_len as i64),
        status: "sending".into(),
        created_at: now,
        ..Default::default()
    };
    s.gw.db
        .insert_message(&msg, std::slice::from_ref(&part))
        .await?;
    audit(&s, &ctx, "sms.raw_pdu", Some(&id), None).await;
    // The submission runs detached: if the HTTP request is cancelled or times
    // out, the outcome is still recorded and the message never stays in
    // `processing`.
    let (gw, modem, part_id, mid) = (s.gw.clone(), m.clone(), part.id.clone(), id.clone());
    let task = tokio::spawn(async move {
        let (state, err, mr) = match modem.send_pdu(tpdu_len, pdu_hex).await {
            Ok(mr) => ("submitted", None, Some(mr as i64)),
            Err(crate::modem::SendError::Ambiguous(e)) => ("submission_unknown", Some(e), None),
            Err(e) => ("failed", Some(e.to_string()), None),
        };
        let part_state = match state {
            "submitted" => "submitted",
            "submission_unknown" => "unknown",
            _ => "failed",
        };
        gw.db
            .update_part(&part_id, part_state, mr, err.as_deref())
            .await?;
        gw.db
            .set_message_state(&mid, state, err.as_deref(), true, None)
            .await?;
        if let Some(r) = mr {
            gw.db.set_modem_ref(&mid, r).await?;
        }
        msg.status = state.into();
        msg.error = err.clone();
        let event = if state == "submitted" {
            "sms.sent"
        } else {
            "sms.failed"
        };
        gw.events
            .emit(event, Some(&mid), crate::events::message_data(&msg))
            .await;
        Ok::<_, crate::database::DbError>((state, err, mr))
    });
    let (state, err, mr) = task.await.map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "submission task failed",
        )
    })??;
    ok(json!({"message_id": id, "status": state, "error": err, "message_reference": mr}))
}

// ---- messages and queue -------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct MessagesQ {
    direction: Option<String>,
    #[serde(rename = "type")]
    message_type: Option<String>,
    status: Option<String>,
    sender: Option<String>,
    recipient: Option<String>,
    modem: Option<String>,
    search: Option<String>,
    from: Option<String>,
    to: Option<String>,
    order: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

fn message_query(q: &MessagesQ, max: i64) -> Result<MessageQuery, ApiError> {
    if let Some(d) = q.direction.as_deref().filter(|d| !d.is_empty()) {
        if d != "inbound" && d != "outbound" {
            return Err(ApiError::bad_request(
                "direction must be inbound or outbound",
            ));
        }
    }
    for (name, v) in [
        ("search", &q.search),
        ("sender", &q.sender),
        ("recipient", &q.recipient),
    ] {
        if v.as_deref()
            .map(|s| s.chars().count() > 200)
            .unwrap_or(false)
        {
            return Err(ApiError::bad_request(format!(
                "{name} is limited to 200 characters"
            )));
        }
    }
    Ok(MessageQuery {
        direction: q.direction.clone(),
        message_type: q.message_type.clone(),
        status: q.status.clone(),
        sender: q.sender.clone(),
        recipient: q.recipient.clone(),
        modem_id: q.modem.clone(),
        search: q.search.clone(),
        from: parse_time("from", &q.from, false)?,
        to: parse_time("to", &q.to, true)?,
        limit: q.limit.unwrap_or(50).clamp(1, max),
        offset: q.offset.unwrap_or(0).max(0),
        ascending: q.order.as_deref() == Some("asc"),
    })
}

/// Message as JSON with address book names added.
fn message_json(s: &AppState, m: &Message) -> Value {
    let cfg = s.gw.cfg.load();
    let mut v = serde_json::to_value(m).unwrap_or(Value::Null);
    v["sender_name"] = json!(m.sender.as_deref().and_then(|n| cfg.contact_name(n)));
    v["recipient_name"] = json!(m.recipient.as_deref().and_then(|n| cfg.contact_name(n)));
    v
}

pub async fn addressbook(State(s): S) -> ApiResult {
    let cfg = s.gw.cfg.load();
    ok(
        json!({"contacts": cfg.contacts, "anonymous": {"count": cfg.anonymous.numbers.len(), "label": cfg.anonymous.label, "redact_body": cfg.anonymous.redact_body}}),
    )
}

fn contact_json(cfg: &crate::config::Config, c: &crate::config::Contact) -> Value {
    json!({
        "id": if c.id.is_empty() { Value::Null } else { json!(c.id) },
        "name": c.name,
        "number": c.number,
        "note": c.note,
        "source": c.source,
        "editable": c.source == "saved",
        "anonymous": cfg.is_anonymous(&c.number),
    })
}

/// Saved contacts and address book entries, ordered by name.
pub async fn contacts(State(s): S) -> ApiResult {
    let cfg = s.gw.cfg.load_full();
    let mut list: Vec<&crate::config::Contact> = cfg.contacts.iter().collect();
    list.sort_by_key(|c| (c.name.to_lowercase(), c.number.clone()));
    ok(json!({"contacts": list.iter().map(|c| contact_json(&cfg, c)).collect::<Vec<_>>()}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContactBody {
    name: String,
    number: String,
    #[serde(default)]
    note: String,
}

fn contact_fields(b: ContactBody) -> Result<(String, String, String), ApiError> {
    let name = b.name.trim().to_string();
    let note = b.note.trim().to_string();
    let number = utils::normalize_number(&b.number);
    if name.is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
        return Err(ApiError::bad_request("name must be 1 to 100 characters"));
    }
    if note.chars().count() > 500 {
        return Err(ApiError::bad_request("note is limited to 500 characters"));
    }
    if !crate::config::valid_number(&number) || number.trim_start_matches('+').len() < 3 {
        return Err(ApiError::bad_request(
            "number must be a telephone number: digits with an optional leading '+'",
        ));
    }
    Ok((name, number, note))
}

fn contact_conflict(e: crate::database::DbError) -> ApiError {
    if e.is_unique_violation() {
        ApiError::conflict("a saved contact with this number already exists")
    } else {
        e.into()
    }
}

async fn saved_contact(s: &AppState, id: &str) -> ApiResult {
    s.gw.refresh_contacts().await?;
    let cfg = s.gw.cfg.load_full();
    let c = cfg
        .contacts
        .iter()
        .find(|c| c.source == "saved" && c.id == id)
        .ok_or_else(|| ApiError::not_found("contact not found"))?;
    ok(contact_json(&cfg, c))
}

pub async fn contact_create(
    State(s): S,
    Extension(ctx): C,
    ApiJson(b): ApiJson<ContactBody>,
) -> Result<Response, ApiError> {
    let (name, number, note) = contact_fields(b)?;
    let id =
        s.gw.db
            .insert_contact(&name, &number, &note)
            .await
            .map_err(contact_conflict)?;
    audit(&s, &ctx, "contact.create", Some(&id), None).await;
    let body = saved_contact(&s, &id).await?;
    Ok((StatusCode::CREATED, body).into_response())
}

pub async fn contact_update(
    State(s): S,
    Extension(ctx): C,
    Path(id): Path<String>,
    ApiJson(b): ApiJson<ContactBody>,
) -> ApiResult {
    let (name, number, note) = contact_fields(b)?;
    let found =
        s.gw.db
            .update_contact(&id, &name, &number, &note)
            .await
            .map_err(contact_conflict)?;
    if !found {
        return Err(ApiError::not_found(
            "contact not found (entries of addressbook.conf are changed in that file)",
        ));
    }
    audit(&s, &ctx, "contact.update", Some(&id), None).await;
    saved_contact(&s, &id).await
}

pub async fn contact_delete(State(s): S, Extension(ctx): C, Path(id): Path<String>) -> ApiResult {
    if !s.gw.db.delete_contact(&id).await? {
        return Err(ApiError::not_found(
            "contact not found (entries of addressbook.conf are changed in that file)",
        ));
    }
    s.gw.refresh_contacts().await?;
    audit(&s, &ctx, "contact.delete", Some(&id), None).await;
    ok(json!({"deleted": true}))
}

pub async fn messages(State(s): S, ApiQuery(q): ApiQuery<MessagesQ>) -> ApiResult {
    let mq = message_query(&q, 500)?;
    let (rows, total) = s.gw.db.search_messages(&mq).await?;
    let rows: Vec<Value> = rows.iter().map(|m| message_json(&s, m)).collect();
    Ok(Json(
        json!({"success": true, "data": rows, "pagination": {"total": total, "limit": mq.limit, "offset": mq.offset}}),
    ))
}

async fn load_message(s: &AppState, id: &str) -> Result<Message, ApiError> {
    if id.len() > 64 {
        return Err(ApiError::not_found("message not found"));
    }
    s.gw.db
        .get_message(id)
        .await?
        .ok_or_else(|| ApiError::not_found("message not found"))
}

pub async fn message(State(s): S, Path(id): Path<String>) -> ApiResult {
    let m = load_message(&s, &id).await?;
    let decoded = m.raw_pdu.as_deref().and_then(|p| {
        let dir = if m.direction == "outbound" {
            PduDirection::MobileOriginated
        } else {
            PduDirection::MobileTerminated
        };
        pdu::decode(p, dir, true).ok()
    });
    let mut v = message_json(&s, &m);
    v["decoded_pdu"] = json!(decoded);
    v["delivery_reports"] = json!(s.gw.db.delivery_reports(&id).await?);
    v["attachments"] = json!(s.gw.db.attachments_for(&id).await?);
    v["queue"] = json!(s.gw.db.queue_item(&id).await?);
    ok(v)
}

pub async fn message_parts(State(s): S, Path(id): Path<String>) -> ApiResult {
    load_message(&s, &id).await?;
    ok(json!(s.gw.db.parts_for_message(&id).await?))
}

pub async fn message_status(State(s): S, Path(id): Path<String>) -> ApiResult {
    let m = load_message(&s, &id).await?;
    ok(json!({
        "message_id": m.id,
        "direction": m.direction,
        "status": m.status,
        "delivery_status": m.delivery_status,
        "error": m.error,
        "retry_count": m.retry_count,
        "submitted_at": m.submitted_at,
        "delivered_at": m.delivered_at,
        "integration_status": m.integration_status,
        "history": s.gw.db.history(&id).await?,
        "delivery_reports": s.gw.db.delivery_reports(&id).await?,
    }))
}

pub async fn message_delete(
    State(s): S,
    Extension(ctx): C,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    require_admin(&s, &ctx, &headers)?;
    let m = load_message(&s, &id).await?;
    if m.status == "processing" {
        return Err(ApiError::conflict(
            "the message is being transmitted; try again shortly",
        ));
    }
    s.gw.db.delete_message(&id).await?;
    audit(&s, &ctx, "message.delete", Some(&id), Some(&m.direction)).await;
    ok(json!({"deleted": true, "message_id": id}))
}

#[derive(Deserialize, Default)]
pub struct QueueQ {
    status: Option<String>,
}

pub async fn queue(
    State(s): S,
    ApiQuery(q): ApiQuery<QueueQ>,
    ApiQuery(p): ApiQuery<Page>,
) -> ApiResult {
    let (rows, total) =
        s.gw.db
            .list_queue(q.status.as_deref(), p.limit(), p.offset())
            .await?;
    paged(json!(rows), total, &p)
}

pub async fn queue_item(State(s): S, Path(id): Path<String>) -> ApiResult {
    let q =
        s.gw.db
            .queue_item(&id)
            .await?
            .ok_or_else(|| ApiError::not_found("queue entry not found"))?;
    ok(json!({"queue": q, "parts": s.gw.db.parts_for_message(&id).await?}))
}

pub async fn queue_retry(
    State(s): S,
    Extension(ctx): C,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    require_admin(&s, &ctx, &headers)?;
    let q =
        s.gw.db
            .queue_item(&id)
            .await?
            .ok_or_else(|| ApiError::not_found("queue entry not found"))?;
    let expiry = s.gw.cfg.load().sms.queue_expiry_seconds;
    let expires = (expiry > 0).then(|| utils::ts_after(expiry as i64));
    if !s.gw.db.retry_message(&id, expires).await? {
        return Err(ApiError::conflict(format!(
            "a message in state {:?} cannot be retried",
            q.status
        )));
    }
    audit(
        &s,
        &ctx,
        "queue.retry",
        Some(&id),
        Some(&format!("previous state {}", q.status)),
    )
    .await;
    s.gw.sms.wake.notify_waiters();
    ok(json!({"message_id": id, "status": "queued"}))
}

pub async fn queue_cancel(State(s): S, Extension(ctx): C, Path(id): Path<String>) -> ApiResult {
    let q =
        s.gw.db
            .queue_item(&id)
            .await?
            .ok_or_else(|| ApiError::not_found("queue entry not found"))?;
    if !s.gw.db.cancel_queued(&id).await? {
        return Err(ApiError::conflict(format!(
            "a message in state {:?} can no longer be cancelled",
            q.status
        )));
    }
    audit(&s, &ctx, "queue.cancel", Some(&id), None).await;
    ok(json!({"message_id": id, "status": "cancelled"}))
}

// ---- PDU tools (offline; never transmit) --------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncodeBody {
    to: String,
    message: Option<String>,
    binary: Option<String>,
    #[serde(default)]
    encoding: Encoding,
    smsc: Option<String>,
    #[serde(default)]
    flash: bool,
    message_class: Option<u8>,
    #[serde(default)]
    delivery_report: bool,
    validity_minutes: Option<u32>,
    protocol_identifier: Option<u8>,
    destination_port: Option<u16>,
    source_port: Option<u16>,
    reference: Option<u8>,
}

pub async fn pdu_encode(ApiJson(b): ApiJson<EncodeBody>) -> ApiResult {
    let payload = match (b.message, b.binary) {
        (Some(t), None) => Payload::Text(t),
        (None, Some(h)) => Payload::Binary(
            hex::decode(h.trim())
                .map_err(|_| ApiError::bad_request("binary must be hexadecimal"))?,
        ),
        _ => {
            return Err(ApiError::bad_request(
                "provide exactly one of message or binary",
            ))
        }
    };
    let req = SubmitRequest {
        to: utils::normalize_number(&b.to),
        smsc: b.smsc.filter(|s| !s.is_empty()),
        payload,
        encoding: b.encoding,
        allow_unicode: true,
        message_class: if b.flash { Some(0) } else { b.message_class },
        status_report: b.delivery_report,
        validity_minutes: b.validity_minutes,
        protocol_identifier: b.protocol_identifier.unwrap_or(0),
        destination_port: b.destination_port,
        source_port: b.source_port,
        reference: b.reference.unwrap_or(0),
        max_parts: 255,
        reject_duplicates: false,
    };
    let enc = pdu::encode_submit(&req).map_err(|e| ApiError::bad_request(e.to_string()))?;
    ok(json!({"transmitted": false, "encoded": enc}))
}

#[derive(Deserialize)]
pub struct DecodeBody {
    pdu: String,
    /// auto (default), deliver (received from the network) or submit (sent to it)
    direction: Option<String>,
    /// Whether the PDU starts with a service centre address (default true).
    smsc: Option<bool>,
}

pub async fn pdu_decode(ApiJson(b): ApiJson<DecodeBody>) -> ApiResult {
    let r = match b.direction.as_deref().unwrap_or("auto") {
        "auto" => pdu::decode_auto(&b.pdu),
        "deliver" | "mt" => pdu::decode(
            &b.pdu,
            PduDirection::MobileTerminated,
            b.smsc.unwrap_or(true),
        ),
        "submit" | "mo" => pdu::decode(
            &b.pdu,
            PduDirection::MobileOriginated,
            b.smsc.unwrap_or(true),
        ),
        _ => {
            return Err(ApiError::bad_request(
                "direction must be auto, deliver or submit",
            ))
        }
    };
    let decoded = r.map_err(|e| {
        ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_pdu",
            e.to_string(),
        )
    })?;
    let mms = match &decoded {
        pdu::Pdu::Deliver(d) => crate::mms::parse_wap_push(&d.user_data.payload),
        _ => None,
    };
    ok(json!({"decoded": decoded, "mms_notification": mms}))
}

// ---- calls and recordings -----------------------------------------------------

pub async fn calls(State(s): S, ApiQuery(p): ApiQuery<Page>) -> ApiResult {
    let (rows, total) = s.gw.db.list_calls(p.limit(), p.offset()).await?;
    paged(json!(rows), total, &p)
}

pub async fn call(State(s): S, Path(id): Path<String>) -> ApiResult {
    ok(s.gw
        .db
        .get_call(&id)
        .await?
        .ok_or_else(|| ApiError::not_found("call not found"))?)
}

pub async fn recordings(State(s): S, ApiQuery(p): ApiQuery<Page>) -> ApiResult {
    let (rows, total) = s.gw.db.list_recordings(p.limit(), p.offset()).await?;
    paged(json!(rows), total, &p)
}

pub async fn recording_download(
    State(s): S,
    Extension(ctx): C,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let path =
        s.gw.db
            .recording_path(&id)
            .await?
            .ok_or_else(|| ApiError::not_found("recording not found"))?;
    let root = std::fs::canonicalize(&s.gw.cfg.load().paths.recordings)
        .map_err(|_| ApiError::not_found("recording file is not available"))?;
    // Only files inside the recordings directory are ever served.
    let real = std::fs::canonicalize(&path)
        .map_err(|_| ApiError::not_found("recording file is not available"))?;
    if !real.starts_with(&root) {
        return Err(ApiError::forbidden(
            "recording is outside the recordings directory",
        ));
    }
    let bytes = tokio::fs::read(&real)
        .await
        .map_err(|_| ApiError::not_found("recording file is not available"))?;
    audit(&s, &ctx, "recording.download", Some(&id), None).await;
    let mut r = text(StatusCode::OK, "audio/wav", bytes);
    if let Ok(v) = HeaderValue::from_str(&format!(
        "attachment; filename=\"recording-{}.wav\"",
        utils::sanitize_filename(&id)
    )) {
        r.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(r)
}

/// Download a stored attachment (for example a retrieved MMS). The content is
/// always served as an opaque download, never rendered.
pub async fn attachment_download(
    State(s): S,
    Extension(ctx): C,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let (path, name, _) =
        s.gw.db
            .attachment_file(&id)
            .await?
            .ok_or_else(|| ApiError::not_found("attachment not found"))?;
    let root = std::fs::canonicalize(&s.gw.cfg.load().paths.attachments)
        .map_err(|_| ApiError::not_found("attachment file is not available"))?;
    let real = std::fs::canonicalize(&path)
        .map_err(|_| ApiError::not_found("attachment file is not available"))?;
    if !real.starts_with(&root) {
        return Err(ApiError::forbidden(
            "attachment is outside the attachments directory",
        ));
    }
    let bytes = tokio::fs::read(&real)
        .await
        .map_err(|_| ApiError::not_found("attachment file is not available"))?;
    audit(&s, &ctx, "attachment.download", Some(&id), None).await;
    let mut r = text(StatusCode::OK, "application/octet-stream", bytes);
    if let Ok(v) = HeaderValue::from_str(&format!(
        "attachment; filename=\"{}\"",
        utils::sanitize_filename(&name)
    )) {
        r.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(r)
}

// ---- integrations ---------------------------------------------------------------

pub async fn integrations(State(s): S) -> ApiResult {
    let cfg = s.gw.cfg.load_full();
    let mut out = Vec::new();
    for i in &cfg.integration {
        out.push(json!({
            "name": i.name,
            "type": i.kind,
            "enabled": i.enabled && cfg.integrations.enabled,
            "events": i.events,
            "method": if i.kind == "webhook" { Some(i.method.to_uppercase()) } else { None },
            "stats": s.gw.db.integration_stats(&i.name).await?,
        }));
    }
    ok(
        json!({"enabled": cfg.integrations.enabled, "integrations": out, "routing_rules": cfg.routing.iter().map(|r| json!({"name": r.name, "enabled": r.enabled, "events": r.events, "destinations": r.destinations, "stop_processing": r.stop_processing})).collect::<Vec<_>>(), "automation_rules": cfg.automation.iter().map(|r| json!({"name": r.name, "enabled": r.enabled, "action": r.action})).collect::<Vec<_>>()}),
    )
}

/// Configured SMS providers and their usage. Secrets are never included.
pub async fn providers(State(s): S) -> ApiResult {
    let cfg = s.gw.cfg.load_full();
    let (day, month) = crate::sms::quota_windows(&cfg, Utc::now());
    let mut out = Vec::new();
    for p in &cfg.providers {
        let route = crate::providers::route_id(&p.name);
        let db = &s.gw.db;
        out.push(json!({
            "name": p.name,
            "type": p.kind,
            "enabled": p.enabled,
            "route": route,
            "default_sender": p.default_sender,
            "allowed_senders": p.allowed_senders,
            "any_sender_allowed": p.allowed_senders.iter().any(|a| a == "*"),
            "delivery_callbacks": !p.callback_secret.is_empty(),
            "callback_path": format!("/api/v1/providers/{}/callback", p.name),
            "daily_sms_limit": p.daily_sms_limit,
            "monthly_sms_limit": p.monthly_sms_limit,
            "segments_today": db.segments_submitted_since(&route, &utils::ts(day)).await?,
            "segments_this_month": db.segments_submitted_since(&route, &utils::ts(month)).await?,
        }));
    }
    ok(json!({"providers": out}))
}

/// Delivery status webhook of an SMS provider. The request is authenticated
/// by its signature alone, so it is exempt from the address and token checks.
pub async fn provider_callback(
    State(s): S,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> ApiResult {
    use crate::providers::CallbackError as E;
    let signature = headers.get("signature").and_then(|h| h.to_str().ok());
    match crate::providers::handle_callback(&s.gw.sms, &name, signature, &body).await {
        Ok(result) => {
            tracing::debug!(provider = %name, result, "provider callback");
            ok(json!({"result": result}))
        }
        Err(E::Disabled) => Err(ApiError::not_found(
            "no such provider, or it has no callback_secret",
        )),
        Err(E::BadSignature) => {
            tracing::warn!(provider = %name, "provider callback rejected: missing or invalid signature");
            Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "invalid_signature",
                "the Signature header is missing or does not match",
            ))
        }
        Err(E::Invalid(m)) => Err(ApiError::bad_request(m)),
        Err(E::NotYet) => Err(ApiError::unavailable(
            "the message is still being submitted; repeat the notification later",
        )),
        Err(E::Database(e)) => Err(e.into()),
    }
}

pub async fn integration_status(State(s): S, Path(name): Path<String>) -> ApiResult {
    let cfg = s.gw.cfg.load_full();
    let i = cfg
        .integration_def(&name)
        .ok_or_else(|| ApiError::not_found("integration not found"))?;
    let (recent, _) = s.gw.db.list_deliveries(Some(&name), None, 20, 0).await?;
    ok(
        json!({"name": i.name, "type": i.kind, "enabled": i.enabled && cfg.integrations.enabled, "events": i.events, "stats": s.gw.db.integration_stats(&name).await?, "recent_deliveries": recent}),
    )
}

/// Deliver a synthetic test event directly (bypassing the queue).
pub async fn integration_test(
    State(s): S,
    Extension(ctx): C,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> ApiResult {
    require_admin(&s, &ctx, &headers)?;
    let cfg = s.gw.cfg.load_full();
    let def = cfg
        .integration_def(&name)
        .ok_or_else(|| ApiError::not_found("integration not found"))?;
    let ev = crate::database::EventRecord {
        id: utils::new_id(),
        event_type: "integration.test".into(),
        instance_id: cfg.general.instance_id.clone(),
        correlation_id: Some(ctx.request_id.clone()),
        timestamp: utils::now(),
        data: json!({"test": true, "message": {"id": "00000000-0000-0000-0000-000000000000", "direction": "inbound", "type": "sms", "sender": "+4500000000", "recipient": null, "body": "Test message from THUGSred SMS Gateway", "encoding": "gsm7", "parts": 1}}),
    };
    audit(&s, &ctx, "integration.test", Some(&name), None).await;
    let outcome = s.gw.integration_sender.deliver(def, &ev, &cfg).await;
    tracing::info!(target: "integrations", integration = %name, success = outcome.success, status_code = ?outcome.status_code, error = ?outcome.error, "test delivery");
    ok(json!({"integration": name, "enabled": def.enabled, "outcome": outcome}))
}

#[derive(Deserialize, Default)]
pub struct DeliveriesQ {
    integration: Option<String>,
    status: Option<String>,
}

pub async fn deliveries(
    State(s): S,
    ApiQuery(q): ApiQuery<DeliveriesQ>,
    ApiQuery(p): ApiQuery<Page>,
) -> ApiResult {
    let (rows, total) =
        s.gw.db
            .list_deliveries(
                q.integration.as_deref(),
                q.status.as_deref(),
                p.limit(),
                p.offset(),
            )
            .await?;
    paged(json!(rows), total, &p)
}

pub async fn delivery_replay(
    State(s): S,
    Extension(ctx): C,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    require_admin(&s, &ctx, &headers)?;
    if !s.gw.db.replay_delivery(&id).await? {
        return Err(ApiError::conflict(
            "only dead or retrying deliveries can be replayed",
        ));
    }
    audit(&s, &ctx, "integration.replay", Some(&id), None).await;
    s.gw.events.wake_handle().notify_waiters();
    ok(json!({"delivery_id": id, "status": "pending"}))
}

pub async fn dead_letters(State(s): S, ApiQuery(p): ApiQuery<Page>) -> ApiResult {
    let (rows, total) = s.gw.db.list_dead_letters(p.limit(), p.offset()).await?;
    paged(json!(rows), total, &p)
}

// ---- events -----------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct EventsQ {
    #[serde(rename = "type")]
    event_type: Option<String>,
    since: Option<String>,
}

pub async fn events(
    State(s): S,
    ApiQuery(q): ApiQuery<EventsQ>,
    ApiQuery(p): ApiQuery<Page>,
) -> ApiResult {
    let since = parse_time("since", &q.since, false)?;
    let (rows, total) =
        s.gw.db
            .list_events(
                q.event_type.as_deref(),
                since.as_deref(),
                p.limit(),
                p.offset(),
            )
            .await?;
    paged(json!(rows), total, &p)
}

/// Server-sent event stream of gateway events. `type` filters by prefix.
pub async fn events_stream(
    State(s): S,
    ApiQuery(q): ApiQuery<EventsQ>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let prefix = q.event_type.unwrap_or_default();
    let mut shutdown = s.gw.shutdown_signal();
    let stream = BroadcastStream::new(s.gw.events.subscribe()).filter_map(move |item| match item {
        Ok(ev) if ev.event_type.starts_with(&prefix) => Some(Ok(Event::default()
            .event(ev.event_type.clone())
            .id(ev.id.clone())
            .data(serde_json::to_string(&ev).unwrap_or_default()))),
        Ok(_) => None,
        // The subscriber fell behind; tell it that events were skipped.
        Err(_) => Some(Ok(Event::default().event("stream.lagged").data("{}"))),
    });
    let stream = futures::StreamExt::take_until(stream, async move {
        let _ = shutdown.changed().await;
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

// ---- export -------------------------------------------------------------------------

/// Quote a CSV field and neutralise spreadsheet formulas.
pub fn csv_field(v: &str) -> String {
    let mut s = v.to_string();
    if s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        s.insert(0, '\'');
    }
    format!("\"{}\"", s.replace('"', "\"\""))
}

#[derive(Deserialize, Default)]
pub struct ExportQ {
    format: Option<String>,
}

pub async fn export(
    State(s): S,
    Extension(ctx): C,
    ApiQuery(q): ApiQuery<MessagesQ>,
    ApiQuery(f): ApiQuery<ExportQ>,
) -> Result<Response, ApiError> {
    let mut mq = message_query(&q, 50_000)?;
    if q.limit.is_none() {
        mq.limit = 10_000;
    }
    let format = f.format.as_deref().unwrap_or("json");
    if format != "json" && format != "csv" {
        return Err(ApiError::bad_request("format must be json or csv"));
    }
    let (rows, total) = s.gw.db.search_messages(&mq).await?;
    audit(
        &s,
        &ctx,
        "history.export",
        None,
        Some(&format!("format={format} rows={}", rows.len())),
    )
    .await;
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let (body, ctype, ext) = if format == "csv" {
        let mut out = String::from("id,direction,type,status,sender,recipient,body,encoding,parts,modem,network_timestamp,received_at,submitted_at,delivered_at,created_at,error\r\n");
        for m in &rows {
            let o = |v: &Option<String>| csv_field(v.as_deref().unwrap_or(""));
            out.push_str(
                &[
                    csv_field(&m.id),
                    csv_field(&m.direction),
                    csv_field(&m.message_type),
                    csv_field(&m.status),
                    o(&m.sender),
                    o(&m.recipient),
                    o(&m.body),
                    o(&m.encoding),
                    csv_field(&m.parts.to_string()),
                    o(&m.modem_id),
                    o(&m.network_timestamp),
                    o(&m.received_at),
                    o(&m.submitted_at),
                    o(&m.delivered_at),
                    csv_field(&m.created_at),
                    o(&m.error),
                ]
                .join(","),
            );
            out.push_str("\r\n");
        }
        (out, "text/csv; charset=utf-8", "csv")
    } else {
        (serde_json::to_string_pretty(&json!({"exported_at": utils::now(), "total_matching": total, "count": rows.len(), "messages": rows})).unwrap_or_default(), "application/json", "json")
    };
    let mut r = text(StatusCode::OK, ctype, body);
    if let Ok(v) = HeaderValue::from_str(&format!(
        "attachment; filename=\"smsgateway-messages-{stamp}.{ext}\""
    )) {
        r.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(r)
}

// ---- USSD and GNSS ------------------------------------------------------------------

#[derive(Deserialize)]
pub struct UssdBody {
    /// Name of a command from `[ussd.commands]`.
    command: Option<String>,
    /// Literal USSD code (requires ussd.allow_arbitrary_codes).
    code: Option<String>,
    modem: Option<String>,
}

pub async fn ussd(State(s): S, Extension(ctx): C, ApiJson(b): ApiJson<UssdBody>) -> ApiResult {
    let cfg = s.gw.cfg.load_full();
    if !cfg.ussd.enabled {
        return Err(ApiError::forbidden("USSD is disabled (ussd.enabled)"));
    }
    let code = match (b.command.as_deref(), b.code.as_deref()) {
        (Some(name), None) => cfg.ussd.commands.get(name).cloned().ok_or_else(|| {
            ApiError::not_found(format!("USSD command {name:?} is not configured"))
        })?,
        (None, Some(code)) => {
            if !cfg.ussd.allow_arbitrary_codes && !cfg.ussd.commands.values().any(|c| c == code) {
                return Err(ApiError::forbidden(
                    "only configured USSD commands are allowed (ussd.allow_arbitrary_codes)",
                ));
            }
            code.to_string()
        }
        _ => {
            return Err(ApiError::bad_request(
                "provide exactly one of command or code",
            ))
        }
    };
    let valid = code.len() <= 42
        && code.ends_with('#')
        && code.starts_with(['*', '#'])
        && code
            .chars()
            .all(|c| c.is_ascii_digit() || c == '*' || c == '#');
    if !valid {
        return Err(ApiError::bad_request("not a valid USSD code"));
    }
    let m =
        s.gw.modem(b.modem.as_deref())
            .ok_or_else(|| ApiError::not_found("no such modem"))?;
    audit(&s, &ctx, "ussd.request", Some(&m.id), Some(&code)).await;
    match m.ussd(&code).await {
        Ok(response) => {
            s.gw.events
                .emit(
                    "ussd.response",
                    None,
                    json!({"modem_id": m.id, "ussd": {"code": code, "response": response}}),
                )
                .await;
            ok(json!({"modem": m.id, "code": code, "response": response}))
        }
        Err(e) => Err(ApiError::new(StatusCode::BAD_GATEWAY, "ussd_failed", e)),
    }
}

pub async fn ussd_history(State(s): S, ApiQuery(p): ApiQuery<Page>) -> ApiResult {
    let (rows, total) = s.gw.db.list_ussd(p.limit(), p.offset()).await?;
    paged(json!(rows), total, &p)
}

pub async fn gnss(State(s): S, ApiQuery(p): ApiQuery<Page>) -> ApiResult {
    let cfg = s.gw.cfg.load_full();
    let m =
        s.gw.modem(Some(cfg.gnss.modem.as_str()).filter(|x| !x.is_empty()))
            .ok_or_else(|| ApiError::not_found("no modem configured"))?;
    let st = m.status();
    let history = if cfg.gnss.store_location_history {
        json!(s.gw.db.gnss_history(p.limit()).await?)
    } else {
        Value::Null
    };
    ok(
        json!({"enabled": cfg.gnss.enabled, "modem": st.id, "capability": st.capabilities.gnss, "position": st.gnss, "last_fix_at": st.gnss_last_fix_at, "history": history}),
    )
}

// ---- configuration, audit, diagnostics ----------------------------------------------

pub async fn config_view(State(s): S, Extension(ctx): C, headers: HeaderMap) -> ApiResult {
    require_admin(&s, &ctx, &headers)?;
    ok(s.gw.cfg.load().redacted())
}

pub async fn config_status(State(s): S) -> ApiResult {
    let cfg = s.gw.cfg.load();
    ok(
        json!({"status": s.gw.config_status(), "files": {"main": cfg.files.main, "sim": cfg.files.sim, "integrations": cfg.files.integrations}}),
    )
}

pub async fn config_reload(State(s): S, Extension(ctx): C, headers: HeaderMap) -> ApiResult {
    require_admin(&s, &ctx, &headers)?;
    audit(&s, &ctx, "config.reload", None, None).await;
    match s.gw.reload().await {
        Ok(restart) => ok(json!({"reloaded": true, "restart_required": restart})),
        Err(e) => Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_configuration",
            e.to_string(),
        )),
    }
}

pub async fn audit_log(
    State(s): S,
    Extension(ctx): C,
    headers: HeaderMap,
    ApiQuery(p): ApiQuery<Page>,
) -> ApiResult {
    require_admin(&s, &ctx, &headers)?;
    let (rows, total) = s.gw.db.list_audit(p.limit(), p.offset()).await?;
    paged(json!(rows), total, &p)
}

/// Diagnostic report with every secret redacted.
pub async fn diagnostics(State(s): S, Extension(ctx): C, headers: HeaderMap) -> ApiResult {
    require_admin(&s, &ctx, &headers)?;
    let cfg = s.gw.cfg.load_full();
    let (recent_events, _) = s.gw.db.list_events(None, None, 50, 0).await?;
    // Event payloads may contain message content; only metadata is included.
    let events: Vec<Value> = recent_events
        .iter()
        .map(|e| json!({"type": e.event_type, "timestamp": e.timestamp}))
        .collect();
    audit(&s, &ctx, "diagnostics.export", None, None).await;
    ok(json!({
        "generated_at": utils::now(),
        "version": version_info(),
        "system": {"os": std::env::consts::OS, "arch": std::env::consts::ARCH, "uptime_seconds": s.gw.metrics.uptime_seconds()},
        "config": cfg.redacted(),
        "config_status": s.gw.config_status(),
        "modems": s.gw.modems.iter().map(|m| {
            let mut st = serde_json::to_value(m.status()).unwrap_or(Value::Null);
            for k in ["imei"] {
                if let Some(v) = st.get(k).and_then(Value::as_str).map(|v| format!("…{}", &v[v.len().saturating_sub(4)..])) { st[k] = json!(v); }
            }
            for k in ["iccid", "imsi", "msisdn"] {
                if let Some(v) = st["sim"].get(k).and_then(Value::as_str).map(|v| format!("…{}", &v[v.len().saturating_sub(4)..])) { st["sim"][k] = json!(v); }
            }
            st
        }).collect::<Vec<_>>(),
        "database": {"verify": s.gw.db.verify().await?, "tables": s.gw.db.table_counts().await?, "size_bytes": s.gw.db.size_bytes()},
        "statistics": s.gw.db.stats().await?,
        "disk": crate::metrics::disk_usage(&cfg.paths.data).map(|(f, t)| json!({"free_bytes": f, "total_bytes": t})),
        "recent_events": events,
    }))
}

pub async fn db_backup(State(s): S, Extension(ctx): C, headers: HeaderMap) -> ApiResult {
    require_admin(&s, &ctx, &headers)?;
    let dir = s.gw.cfg.load().paths.backups.clone();
    match s.gw.db.backup(&dir).await {
        Ok(path) => {
            audit(&s, &ctx, "database.backup", Some(&path), None).await;
            ok(json!({"path": path, "verified": true}))
        }
        Err(e) => Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "backup_failed",
            e.to_string(),
        )),
    }
}

/// Settings the dashboard needs. Contains no secrets.
pub async fn ui_config(State(s): S) -> ApiResult {
    let cfg = s.gw.cfg.load();
    ok(json!({
        "title": cfg.web.title,
        "theme": cfg.web.theme,
        "refresh_interval_seconds": cfg.web.refresh_interval_seconds,
        "show_message_content": cfg.web.show_message_content,
        "show_modem_information": cfg.web.show_modem_information,
        "authentication": cfg.api.authentication,
        "instance_name": cfg.general.instance_name,
        "version": daemon::VERSION,
        "max_message_parts": cfg.sms.max_message_parts,
        "ussd_commands": cfg.ussd.commands.keys().collect::<Vec<_>>(),
        "modems": cfg.modems.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
    }))
}
