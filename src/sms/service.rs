//! Outbound submission: validation, policy checks, encoding and queueing.

use super::pdu::{self, Encoding, Payload, SubmitRequest};
use crate::config::{valid_number, Config, SharedConfig};
use crate::database::{Db, Message, Part, QueueItem};
use crate::events::{message_data, Events};
use crate::metrics::Metrics;
use crate::utils;
use chrono::{DateTime, Datelike, TimeZone, Utc};
use std::sync::Arc;
use tokio::sync::Notify;

#[derive(Debug, Clone, Default)]
pub struct SendRequest {
    pub to: String,
    pub message: Option<String>,
    pub binary: Option<Vec<u8>>,
    pub encoding: Encoding,
    pub delivery_report: Option<bool>,
    /// Higher values are sent first (high = 10, normal = 0, low = -10).
    pub priority: i64,
    pub flash: bool,
    pub message_class: Option<u8>,
    pub validity_minutes: Option<u32>,
    pub scheduled_at: Option<DateTime<Utc>>,
    /// Send through this modem.
    pub modem: Option<String>,
    /// Send through the modem whose SIM has this gateway number. This selects
    /// a physical SIM; it cannot change the number the network presents.
    pub from: Option<String>,
    pub destination_port: Option<u16>,
    pub source_port: Option<u16>,
    pub protocol_identifier: Option<u8>,
    pub idempotency_key: Option<String>,
    pub correlation_id: Option<String>,
    /// Bypass `security.destination_allowlist` (administrative callers only,
    /// and only when `security.allow_destination_overrides` is set).
    pub override_allowlist: bool,
    /// api, cli, web or automation:<rule>
    pub origin: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SubmitError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    LimitReached(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("database error: {0}")]
    Database(#[from] crate::database::DbError),
}

#[derive(Debug, Clone)]
pub struct Submitted {
    pub message: Message,
    /// True when an earlier request with the same idempotency key was returned.
    pub duplicate: bool,
}

#[derive(Clone)]
pub struct SmsService {
    pub cfg: SharedConfig,
    pub db: Db,
    pub events: Events,
    pub metrics: Arc<Metrics>,
    /// Wakes the sender workers when new work is queued.
    pub wake: Arc<Notify>,
}

/// Start of the current day and month in the configured time zone, as UTC.
pub fn quota_windows(cfg: &Config, now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    let tz = cfg.timezone();
    let local = now.with_timezone(&tz);
    let day = tz
        .with_ymd_and_hms(local.year(), local.month(), local.day(), 0, 0, 0)
        .earliest()
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or(now);
    let month = tz
        .with_ymd_and_hms(local.year(), local.month(), 1, 0, 0, 0)
        .earliest()
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or(now);
    (day, month)
}

impl SmsService {
    pub fn new(cfg: SharedConfig, db: Db, events: Events, metrics: Arc<Metrics>) -> Self {
        Self {
            cfg,
            db,
            events,
            metrics,
            wake: Arc::new(Notify::new()),
        }
    }

    /// Resolve which modem (if any) a request is pinned to.
    fn select_modem(cfg: &Config, req: &SendRequest) -> Result<Option<String>, SubmitError> {
        if let Some(id) = req.modem.as_deref().filter(|m| !m.is_empty()) {
            return match cfg.modem(id) {
                Some(m) if m.enabled => Ok(Some(id.to_string())),
                Some(_) => Err(SubmitError::Invalid(format!("modem {id:?} is disabled"))),
                None => Err(SubmitError::Invalid(format!("unknown modem {id:?}"))),
            };
        }
        if let Some(from) = req.from.as_deref().filter(|m| !m.is_empty()) {
            let hit = cfg.modems.iter().find(|m| {
                m.enabled
                    && cfg
                        .sim_for_modem(&m.id)
                        .map(|s| {
                            !s.mobile_number.is_empty()
                                && utils::numbers_match(&s.mobile_number, from)
                        })
                        .unwrap_or(false)
            });
            return match hit {
                Some(m) => Ok(Some(m.id.clone())),
                None => Err(SubmitError::Invalid(format!(
                    "no configured SIM has the gateway number {from:?}; `from` selects one of the gateway's own SIMs and cannot set an arbitrary sender"
                ))),
            };
        }
        Ok(None)
    }

    /// Build the encoder request shared by estimation and submission.
    fn encoder_request(
        cfg: &Config,
        req: &SendRequest,
        to: &str,
    ) -> Result<SubmitRequest, SubmitError> {
        let sms = &cfg.sms;
        let payload = match (&req.message, &req.binary) {
            (Some(_), Some(_)) => {
                return Err(SubmitError::Invalid(
                    "provide either message or binary data, not both".into(),
                ))
            }
            (Some(t), None) => {
                if t.is_empty() {
                    return Err(SubmitError::Invalid("message must not be empty".into()));
                }
                Payload::Text(t.clone())
            }
            (None, Some(b)) => {
                if !sms.allow_binary {
                    return Err(SubmitError::Forbidden(
                        "binary SMS is disabled (sms.allow_binary)".into(),
                    ));
                }
                if b.is_empty() {
                    return Err(SubmitError::Invalid(
                        "binary payload must not be empty".into(),
                    ));
                }
                Payload::Binary(b.clone())
            }
            (None, None) => return Err(SubmitError::Invalid("message is required".into())),
        };
        let class = match (req.flash, req.message_class) {
            (true, Some(c)) if c != 0 => {
                return Err(SubmitError::Invalid(
                    "flash messages use message class 0".into(),
                ))
            }
            (true, _) => Some(0),
            (false, c) => c,
        };
        if class == Some(0) && !sms.allow_flash_sms {
            return Err(SubmitError::Forbidden(
                "flash SMS is disabled (sms.allow_flash_sms)".into(),
            ));
        }
        let encoding = match req.encoding {
            Encoding::Auto if matches!(payload, Payload::Binary(_)) => Encoding::Binary,
            Encoding::Auto if !sms.auto_encoding => match sms.default_encoding.as_str() {
                "ucs2" => Encoding::Ucs2,
                _ => Encoding::Gsm7,
            },
            e => e,
        };
        let validity = req
            .validity_minutes
            .or((sms.default_validity_minutes > 0).then_some(sms.default_validity_minutes));
        if validity
            .map(|v| v == 0 || v > 63 * 7 * 1440)
            .unwrap_or(false)
        {
            return Err(SubmitError::Invalid(
                "validity_minutes must be between 1 and 635040".into(),
            ));
        }
        Ok(SubmitRequest {
            to: to.to_string(),
            smsc: None,
            payload,
            encoding,
            allow_unicode: sms.allow_unicode,
            message_class: class,
            status_report: req.delivery_report.unwrap_or(sms.request_delivery_reports),
            validity_minutes: validity,
            protocol_identifier: req.protocol_identifier.unwrap_or(0),
            destination_port: req.destination_port,
            source_port: req.source_port,
            reference: (uuid::Uuid::new_v4().as_u128() & 0xFF) as u8,
            max_parts: if sms.allow_multipart {
                sms.max_message_parts
            } else {
                1
            },
            reject_duplicates: false,
        })
    }

    /// Estimate encoding and segment count for a message.
    pub fn estimate(&self, req: &SendRequest) -> Result<pdu::Estimate, SubmitError> {
        let cfg = self.cfg.load();
        let enc = Self::encoder_request(&cfg, req, "+0")?;
        pdu::estimate(
            &enc.payload,
            enc.encoding,
            enc.allow_unicode,
            req.destination_port.is_some() || req.source_port.is_some(),
        )
        .map_err(|e| SubmitError::Invalid(e.to_string()))
    }

    /// Validate, encode and durably queue an outbound message.
    pub async fn submit(&self, req: SendRequest) -> Result<Submitted, SubmitError> {
        let cfg = self.cfg.load_full();
        if !cfg.sms.enabled || !cfg.sms.send_enabled {
            return Err(SubmitError::Unavailable(
                "sending SMS is disabled (sms.enabled / sms.send_enabled)".into(),
            ));
        }
        if let Some(key) = req.idempotency_key.as_deref() {
            if key.is_empty() || key.len() > 128 || !key.chars().all(|c| c.is_ascii_graphic()) {
                return Err(SubmitError::Invalid(
                    "Idempotency-Key must be 1 to 128 printable ASCII characters".into(),
                ));
            }
            if let Some(existing) = self.db.find_by_idempotency_key(key).await? {
                return Ok(Submitted {
                    message: existing,
                    duplicate: true,
                });
            }
        }
        let to = utils::normalize_number(&req.to);
        if !valid_number(&to) || to.trim_start_matches('+').len() < 3 {
            return Err(SubmitError::Invalid(
                "`to` must be a telephone number: digits with an optional leading '+'".into(),
            ));
        }
        let sec = &cfg.security;
        if sec
            .destination_blocklist
            .iter()
            .any(|p| utils::list_matches(p, &to))
        {
            return Err(SubmitError::Forbidden(
                "destination is blocked (security.destination_blocklist)".into(),
            ));
        }
        if !sec.destination_allowlist.is_empty()
            && !sec
                .destination_allowlist
                .iter()
                .any(|p| utils::list_matches(p, &to))
        {
            let overridden = req.override_allowlist && sec.allow_destination_overrides;
            if !overridden {
                return Err(SubmitError::Forbidden(
                    "destination is not on the allowlist (security.destination_allowlist)".into(),
                ));
            }
        }
        if let Some(at) = req.scheduled_at {
            if at > Utc::now() + chrono::Duration::days(366) {
                return Err(SubmitError::Invalid(
                    "scheduled time is more than one year ahead".into(),
                ));
            }
        }
        let modem_id = Self::select_modem(&cfg, &req)?;
        let enc_req = Self::encoder_request(&cfg, &req, &to)?;
        let encoded =
            pdu::encode_submit(&enc_req).map_err(|e| SubmitError::Invalid(e.to_string()))?;

        if self.db.queue_depth().await? as u64 >= sec.max_outbound_queue_size {
            return Err(SubmitError::LimitReached(
                "outbound queue is full (security.max_outbound_queue_size)".into(),
            ));
        }
        // Spending protection: refuse early when a pinned SIM's quota is exhausted.
        if let Some(id) = &modem_id {
            if let Some(reason) = self
                .quota_exhausted(&cfg, id, encoded.parts.len() as i64)
                .await?
            {
                return Err(SubmitError::LimitReached(reason));
            }
        }

        let now = utils::now();
        let id = utils::new_id();
        let scheduled = req.scheduled_at.filter(|t| *t > Utc::now());
        let status = if scheduled.is_some() {
            "scheduled"
        } else {
            "queued"
        };
        let sim = modem_id.as_deref().and_then(|m| cfg.sim_for_modem(m));
        let message_type = match (&enc_req.payload, enc_req.message_class) {
            (Payload::Binary(_), _) => "binary",
            (_, Some(0)) => "flash",
            _ => "sms",
        };
        let message = Message {
            id: id.clone(),
            direction: "outbound".into(),
            message_type: message_type.into(),
            instance_id: cfg.general.instance_id.clone(),
            modem_id: modem_id.clone(),
            sim_id: sim.map(|s| s.id.clone()),
            sender: sim
                .map(|s| s.mobile_number.clone())
                .filter(|n| !n.is_empty()),
            recipient: Some(to.clone()),
            original_address: Some(req.to.clone()),
            normalized_number: Some(to.clone()),
            body: req.message.clone(),
            raw_pdu: cfg.sms.store_raw_pdu.then(|| encoded.parts[0].pdu.clone()),
            payload: req.binary.clone(),
            encoding: Some(encoded.encoding.as_str().into()),
            message_class: enc_req.message_class.map(i64::from),
            protocol_id: Some(enc_req.protocol_identifier as i64),
            data_coding: Some(encoded.data_coding_scheme as i64),
            concat_ref: encoded.reference.map(i64::from),
            concat_total: (encoded.parts.len() > 1).then_some(encoded.parts.len() as i64),
            parts: encoded.parts.len() as i64,
            src_port: req.source_port.map(i64::from),
            dst_port: req.destination_port.map(i64::from),
            status: status.into(),
            metadata: Some(
                serde_json::json!({"origin": req.origin, "validity_minutes": enc_req.validity_minutes}),
            ),
            correlation_id: req.correlation_id.clone(),
            idempotency_key: req.idempotency_key.clone(),
            delivery_report: enc_req.status_report,
            created_at: now.clone(),
            updated_at: now.clone(),
            ..Default::default()
        };
        let parts: Vec<Part> = encoded
            .parts
            .iter()
            .map(|p| Part {
                id: utils::new_id(),
                message_id: Some(id.clone()),
                direction: "outbound".into(),
                modem_id: modem_id.clone(),
                address: Some(to.clone()),
                concat_ref: encoded.reference.map(i64::from),
                concat_total: Some(encoded.parts.len() as i64),
                sequence: p.sequence as i64,
                encoding: Some(encoded.encoding.as_str().into()),
                pdu: Some(p.pdu.clone()),
                payload: Some(p.payload.clone()),
                tpdu_length: Some(p.tpdu_length as i64),
                status: "queued".into(),
                created_at: now.clone(),
                ..Default::default()
            })
            .collect();
        let expires = (cfg.sms.queue_expiry_seconds > 0).then(|| {
            let base = scheduled.unwrap_or_else(Utc::now);
            utils::ts(base + chrono::Duration::seconds(cfg.sms.queue_expiry_seconds as i64))
        });
        let queue = QueueItem {
            message_id: id.clone(),
            status: status.into(),
            priority: req.priority.clamp(-100, 100),
            modem_id,
            not_before: scheduled.map(utils::ts),
            expires_at: expires,
            created_at: now.clone(),
            updated_at: now,
            ..Default::default()
        };
        match self.db.enqueue(&message, &parts, &queue).await {
            Ok(()) => {}
            Err(e) if e.is_unique_violation() => {
                // A concurrent request with the same idempotency key won.
                if let Some(key) = req.idempotency_key.as_deref() {
                    if let Some(existing) = self.db.find_by_idempotency_key(key).await? {
                        return Ok(Submitted {
                            message: existing,
                            duplicate: true,
                        });
                    }
                }
                return Err(e.into());
            }
            Err(e) => return Err(e.into()),
        }
        self.log_message("queued", &message, &cfg);
        self.events
            .emit("sms.queued", Some(&id), message_data(&message))
            .await;
        self.wake.notify_waiters();
        Ok(Submitted {
            message,
            duplicate: false,
        })
    }

    /// Returns a reason when sending `segments` more would exceed a SIM limit.
    pub(crate) async fn quota_exhausted(
        &self,
        cfg: &Config,
        modem_id: &str,
        segments: i64,
    ) -> Result<Option<String>, crate::database::DbError> {
        let Some(sim) = cfg.sim_for_modem(modem_id) else {
            return Ok(None);
        };
        let (day, month) = quota_windows(cfg, Utc::now());
        if sim.limits.daily_sms_limit > 0 {
            let used = self
                .db
                .segments_submitted_since(modem_id, &utils::ts(day))
                .await?;
            if (used + segments) as u64 > sim.limits.daily_sms_limit {
                return Ok(Some(format!(
                    "daily SMS limit of {} reached for SIM {:?}",
                    sim.limits.daily_sms_limit, sim.id
                )));
            }
        }
        if sim.limits.monthly_sms_limit > 0 {
            let used = self
                .db
                .segments_submitted_since(modem_id, &utils::ts(month))
                .await?;
            if (used + segments) as u64 > sim.limits.monthly_sms_limit {
                return Ok(Some(format!(
                    "monthly SMS limit of {} reached for SIM {:?}",
                    sim.limits.monthly_sms_limit, sim.id
                )));
            }
        }
        Ok(None)
    }

    /// Write a message lifecycle entry to the message log. Bodies are only
    /// included when `logging.log_message_content` is enabled.
    pub(crate) fn log_message(&self, action: &str, m: &Message, cfg: &Config) {
        let redact = cfg.logging.redact_sensitive_values;
        // Numbers on the anonymous list never appear in logs.
        let num = |n: &Option<String>| {
            n.as_deref().map(|v| {
                if cfg.is_anonymous(v) {
                    cfg.anonymous.label.clone()
                } else if redact {
                    utils::mask_number(v)
                } else {
                    v.to_string()
                }
            })
        };
        let body = cfg
            .logging
            .log_message_content
            .then(|| m.body.clone())
            .flatten();
        // Optional values are logged as plain strings ("-" when absent).
        let opt = |v: &Option<String>| v.clone().unwrap_or_else(|| "-".into());
        tracing::info!(
            target: "messages",
            action,
            message_id = %m.id,
            direction = %m.direction,
            message_type = %m.message_type,
            status = %m.status,
            modem = %opt(&m.modem_id),
            sender = %opt(&num(&m.sender)),
            recipient = %opt(&num(&m.recipient)),
            parts = m.parts,
            encoding = %opt(&m.encoding),
            correlation_id = %opt(&m.correlation_id),
            error = %opt(&m.error),
            body = %opt(&body),
            "message {action}"
        );
    }
}
