//! Upstream SMS providers: additional outbound routes that hand messages to
//! an HTTP API instead of a modem. A provider can present an approved sender
//! name, which a SIM cannot.
//!
//! Provider messages use the same durable queue as modem messages. Their
//! route is stored in the `modem_id` column as `provider:<name>`, so modem
//! workers never claim them.

pub mod gatewayapi;

use crate::config::{Config, ProviderDef};
use crate::database::{DbError, QueueItem};
use crate::events::message_data;
use crate::sms::SmsService;
use crate::utils;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use tokio::sync::watch;

pub const ROUTE_PREFIX: &str = "provider:";

/// Queue route identifier of a provider.
pub fn route_id(name: &str) -> String {
    format!("{ROUTE_PREFIX}{name}")
}

/// Provider name of a queue route, if the route is a provider.
pub fn provider_of(route: &str) -> Option<&str> {
    route.strip_prefix(ROUTE_PREFIX)
}

/// Why a provider did not accept a message.
#[derive(Debug)]
pub enum SendFailure {
    /// The provider refused the message; repeating it cannot succeed.
    Permanent(String),
    /// The message was certainly not accepted; a later attempt may succeed.
    Retry(String),
    /// The request may have been accepted. Retrying could send (and bill)
    /// the message twice.
    Ambiguous(String),
}

/// A message handed to a provider.
pub struct Outgoing<'a> {
    pub to: &'a str,
    pub sender: &'a str,
    pub text: &'a str,
    /// Echoed back in status callbacks (the gateway's message id).
    pub reference: &'a str,
    pub validity_minutes: Option<u64>,
}

/// One worker serves every configured provider. Providers are read from the
/// live configuration on each pass, so a reload applies without a restart.
pub struct ProviderWorker {
    svc: SmsService,
    http: reqwest::Client,
    /// Submission times within the last minute, per provider.
    windows: HashMap<String, VecDeque<Instant>>,
}

impl ProviderWorker {
    pub fn new(svc: SmsService) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("thugsred-smsgateway/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default();
        Self {
            svc,
            http,
            windows: HashMap::new(),
        }
    }

    fn rate_limited(&mut self, def: &ProviderDef) -> bool {
        if def.rate_limit_per_minute == 0 {
            return false;
        }
        let w = self.windows.entry(def.name.clone()).or_default();
        let now = Instant::now();
        while w
            .front()
            .map(|t| now.duration_since(*t) >= Duration::from_secs(60))
            .unwrap_or(false)
        {
            w.pop_front();
        }
        w.len() >= def.rate_limit_per_minute as usize
    }

    pub async fn run(mut self, mut shutdown: watch::Receiver<bool>) {
        loop {
            if *shutdown.borrow() {
                return;
            }
            let pause = match self.step().await {
                Ok(true) => continue,
                Ok(false) => Duration::from_secs(2),
                Err(e) => {
                    tracing::error!(error = %e, "provider queue error");
                    Duration::from_secs(5)
                }
            };
            tokio::select! {
                _ = tokio::time::sleep(pause) => {}
                _ = self.svc.wake.notified() => {}
                _ = shutdown.changed() => {}
            }
        }
    }

    /// Process at most one message. Returns whether one was processed.
    async fn step(&mut self) -> Result<bool, DbError> {
        let cfg = self.svc.cfg.load_full();
        if !cfg.sms.enabled || !cfg.sms.send_enabled {
            return Ok(false);
        }
        for def in cfg.providers.iter().filter(|p| p.enabled) {
            if self.rate_limited(def) {
                continue;
            }
            let route = route_id(&def.name);
            if let Some(item) = self.svc.db.claim_next(&route, false).await? {
                self.send(&cfg, def, item).await?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn send(
        &mut self,
        cfg: &Config,
        def: &ProviderDef,
        item: QueueItem,
    ) -> Result<(), DbError> {
        let db = self.svc.db.clone();
        let mid = item.message_id.clone();
        let route = route_id(&def.name);
        let Some(message) = db.get_message(&mid).await? else {
            return db
                .set_message_state(&mid, "failed", Some("message record missing"), false, None)
                .await;
        };
        if item
            .expires_at
            .as_deref()
            .map(|e| e < utils::now().as_str())
            .unwrap_or(false)
        {
            db.set_message_state(&mid, "expired", Some("not sent before expiry"), false, None)
                .await?;
            return self.failed_event(&mid).await;
        }
        let (Some(to), Some(sender), Some(text)) = (
            message.recipient.as_deref(),
            message.sender.as_deref(),
            message.body.as_deref(),
        ) else {
            db.set_message_state(
                &mid,
                "failed",
                Some("stored message content is incomplete"),
                false,
                None,
            )
            .await?;
            return self.failed_event(&mid).await;
        };
        // Spending protection. Deferred messages do not consume an attempt.
        if let Some(reason) = self
            .svc
            .provider_quota_exhausted(cfg, def, message.parts.max(1))
            .await?
        {
            tracing::warn!(provider = %def.name, message_id = %mid, "{reason}; message deferred");
            db.add_history(&mid, "queued", Some(&format!("deferred: {reason}")))
                .await?;
            return db.release(&mid, Some(utils::ts_after(600))).await;
        }
        if cfg.sms.per_destination_interval_seconds > 0 {
            if let Some(last) = db
                .last_submission_to(to)
                .await?
                .and_then(|t| utils::parse_ts(&t))
            {
                let next = last
                    + chrono::Duration::seconds(cfg.sms.per_destination_interval_seconds as i64);
                if next > chrono::Utc::now() {
                    return db.release(&mid, Some(utils::ts(next))).await;
                }
            }
        }
        let validity = message
            .metadata
            .as_ref()
            .and_then(|m| m.get("validity_minutes"))
            .and_then(|v| v.as_u64());
        // Persist "sending" first: if the daemon dies during the request,
        // recovery knows the outcome is uncertain.
        db.update_all_parts(&mid, "sending", None).await?;
        let out = Outgoing {
            to,
            sender,
            text,
            reference: &mid,
            validity_minutes: validity,
        };
        let result = match def.kind.as_str() {
            "gatewayapi" => gatewayapi::send(&self.http, def, &out).await,
            other => Err(SendFailure::Permanent(format!(
                "unsupported provider type {other:?}"
            ))),
        };
        match result {
            Ok(provider_id) => {
                db.update_all_parts(&mid, "submitted", None).await?;
                if let Some(pid) = provider_id {
                    db.merge_metadata(&mid, serde_json::json!({"provider_message_id": pid}))
                        .await?;
                }
                db.set_message_state(&mid, "submitted", None, false, None)
                    .await?;
                self.windows
                    .entry(def.name.clone())
                    .or_default()
                    .push_back(Instant::now());
                self.svc
                    .metrics
                    .inc("smsgw_messages_submitted_total", &[("modem", &route)]);
                if let Some(m) = db.get_message(&mid).await? {
                    self.svc.log_message("submitted", &m, cfg);
                    self.svc
                        .events
                        .emit("sms.sent", Some(&mid), message_data(&m))
                        .await;
                }
                if !cfg.sms.store_sent_messages {
                    db.scrub_message_content(&mid).await?;
                }
                Ok(())
            }
            Err(SendFailure::Retry(why)) => {
                db.update_all_parts(&mid, "failed", Some(&why)).await?;
                let attempt = item.attempts + 1;
                if attempt >= cfg.sms.max_send_attempts as i64 {
                    db.set_message_state(&mid, "failed", Some(&why), true, None)
                        .await?;
                    db.add_dead_letter("sms", &mid, &why, None).await?;
                    return self.failed_event(&mid).await;
                }
                let delay = utils::backoff_seconds(
                    cfg.sms.send_retry_delay_seconds.max(1),
                    3600,
                    attempt as u32,
                );
                tracing::warn!(target: "messages", message_id = %mid, provider = %def.name, attempt, retry_in = delay, error = %why, "provider submission failed; will retry");
                db.set_message_state(
                    &mid,
                    "retrying",
                    Some(&why),
                    true,
                    Some(utils::ts_after(delay as i64)),
                )
                .await
            }
            Err(SendFailure::Permanent(why)) => {
                db.update_all_parts(&mid, "failed", Some(&why)).await?;
                db.set_message_state(&mid, "failed", Some(&why), true, None)
                    .await?;
                db.add_dead_letter("sms", &mid, &why, None).await?;
                self.failed_event(&mid).await
            }
            Err(SendFailure::Ambiguous(why)) => {
                db.update_all_parts(&mid, "unknown", Some(&why)).await?;
                db.set_message_state(
                    &mid,
                    "submission_unknown",
                    Some(&format!("{why}; not retried automatically")),
                    true,
                    None,
                )
                .await?;
                self.failed_event(&mid).await
            }
        }
    }

    async fn failed_event(&self, mid: &str) -> Result<(), DbError> {
        if let Some(m) = self.svc.db.get_message(mid).await? {
            let cfg = self.svc.cfg.load_full();
            self.svc.log_message("failed", &m, &cfg);
            self.svc.metrics.inc(
                "smsgw_message_failures_total",
                &[
                    ("modem", m.modem_id.as_deref().unwrap_or("")),
                    ("reason", &m.status),
                ],
            );
            self.svc
                .events
                .emit("sms.failed", Some(mid), message_data(&m))
                .await;
        }
        Ok(())
    }
}

/// A delivery status reported by a provider for one message.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusUpdate {
    /// Unique id of the notification (used to ignore repeats).
    pub event_id: String,
    /// The gateway's message id, as passed to the provider on submission.
    pub reference: String,
    pub provider_message_id: String,
    pub recipient: String,
    /// The provider's own status word.
    pub status: String,
    /// `delivered`, `failed` or `pending`.
    pub category: &'static str,
    pub detail: Option<String>,
    pub status_at: Option<String>,
}

#[derive(Debug)]
pub enum CallbackError {
    /// No such provider, or it has no callback secret.
    Disabled,
    BadSignature,
    Invalid(String),
    /// The message is still being submitted; the provider should repeat the
    /// notification later.
    NotYet,
    Database(DbError),
}

impl From<DbError> for CallbackError {
    fn from(e: DbError) -> Self {
        Self::Database(e)
    }
}

/// Verify `v1=<hex HMAC-SHA256 of the raw body>` in constant time.
pub fn signature_valid(secret: &str, header: &str, body: &[u8]) -> bool {
    let Some(hex_sig) = header.trim().strip_prefix("v1=") else {
        return false;
    };
    let Ok(sig) = hex::decode(hex_sig.trim()) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&sig).is_ok()
}

/// Handle a signed status callback from a provider. Returns a short word
/// describing what was done with it.
pub async fn handle_callback(
    svc: &SmsService,
    name: &str,
    signature: Option<&str>,
    body: &[u8],
) -> Result<&'static str, CallbackError> {
    let cfg = svc.cfg.load_full();
    let def = cfg
        .provider(name)
        .filter(|p| !p.callback_secret.is_empty())
        .ok_or(CallbackError::Disabled)?;
    if !signature
        .map(|s| signature_valid(&def.callback_secret, s, body))
        .unwrap_or(false)
    {
        return Err(CallbackError::BadSignature);
    }
    let update = match def.kind.as_str() {
        "gatewayapi" => gatewayapi::parse_callback(body).map_err(CallbackError::Invalid)?,
        _ => return Err(CallbackError::Disabled),
    };
    let Some(update) = update else {
        return Ok("ignored");
    };
    apply_status(svc, &cfg, def, update).await
}

async fn apply_status(
    svc: &SmsService,
    cfg: &Config,
    def: &ProviderDef,
    u: StatusUpdate,
) -> Result<&'static str, CallbackError> {
    let db = &svc.db;
    let route = route_id(&def.name);
    let Some(m) = db.get_message(&u.reference).await? else {
        return Ok("unknown_message");
    };
    if m.direction != "outbound" || m.modem_id.as_deref() != Some(route.as_str()) {
        return Ok("unknown_message");
    }
    match m.status.as_str() {
        "submitted" | "submission_unknown" => {}
        // The callback overtook our own bookkeeping of the submission.
        "processing" => return Err(CallbackError::NotYet),
        // Final states are never overwritten; late or repeated reports are dropped.
        _ => return Ok("already_final"),
    }
    let description = match &u.detail {
        Some(d) => format!("{}: {d}", u.status),
        None => u.status.clone(),
    };
    if u.category == "pending" {
        db.add_history(
            &m.id,
            &m.status,
            Some(&format!("provider status: {description}")),
        )
        .await?;
        return Ok("noted");
    }
    let fingerprint = utils::sha256_hex(format!("{route}|{}|{}", u.event_id, u.status).as_bytes());
    match db
        .insert_delivery_report(
            Some(&m.id),
            None,
            &route,
            0,
            &u.recipient,
            if u.category == "delivered" { 0 } else { 64 },
            u.category,
            &description,
            None,
            u.status_at.clone(),
            None,
            &fingerprint,
        )
        .await
    {
        Ok(_) => {}
        Err(e) if e.is_unique_violation() => return Ok("duplicate"),
        Err(e) => return Err(e.into()),
    }
    let (state, error) = if u.category == "delivered" {
        ("delivered", None)
    } else {
        (
            "delivery_failed",
            Some(format!("delivery failed: {description}")),
        )
    };
    db.update_all_parts(&m.id, state, error.as_deref()).await?;
    db.set_message_state(&m.id, state, error.as_deref(), false, None)
        .await?;
    if let Some(m) = db.get_message(&m.id).await? {
        svc.log_message(state, &m, cfg);
        if state == "delivered" {
            svc.metrics
                .inc("smsgw_messages_delivered_total", &[("modem", &route)]);
            svc.events
                .emit("sms.delivered", Some(&m.id), message_data(&m))
                .await;
        } else {
            svc.metrics.inc(
                "smsgw_message_failures_total",
                &[("modem", &route), ("reason", "delivery")],
            );
            svc.events
                .emit("sms.failed", Some(&m.id), message_data(&m))
                .await;
        }
    }
    Ok("applied")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes() {
        assert_eq!(route_id("gw"), "provider:gw");
        assert_eq!(provider_of("provider:gw"), Some("gw"));
        assert_eq!(provider_of("modem-1"), None);
    }

    #[test]
    fn signature() {
        let body = br#"{"event_id":"e1"}"#;
        let mut mac = Hmac::<Sha256>::new_from_slice(b"secret").unwrap();
        mac.update(body);
        let good = format!("v1={}", hex::encode(mac.finalize().into_bytes()));
        assert!(signature_valid("secret", &good, body));
        assert!(!signature_valid("other", &good, body));
        assert!(!signature_valid("secret", &good, b"{}"));
        assert!(!signature_valid("secret", &good[3..], body));
        assert!(!signature_valid("secret", "v1=zz", body));
    }
}
