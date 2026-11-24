//! Outbound queue worker. One worker runs per modem and is the only place
//! that hands PDUs to that modem.

use super::service::SmsService;
use crate::database::QueueItem;
use crate::events::message_data;
use crate::modem::{Modem, SendError};
use crate::utils;
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tokio::sync::watch;

pub struct Sender {
    pub svc: SmsService,
    pub modem: Modem,
    /// Submission times within the last minute (rate limiting).
    window: VecDeque<Instant>,
}

impl Sender {
    pub fn new(svc: SmsService, modem: Modem) -> Self {
        Self {
            svc,
            modem,
            window: VecDeque::new(),
        }
    }

    /// Seconds until the per-minute rate limit allows another segment.
    fn rate_wait(&mut self, limit: u32) -> Option<Duration> {
        if limit == 0 {
            return None;
        }
        let now = Instant::now();
        while self
            .window
            .front()
            .map(|t| now.duration_since(*t) >= Duration::from_secs(60))
            .unwrap_or(false)
        {
            self.window.pop_front();
        }
        if self.window.len() >= limit as usize {
            let oldest = *self.window.front()?;
            return Some(
                Duration::from_secs(60).saturating_sub(now.duration_since(oldest))
                    + Duration::from_millis(50),
            );
        }
        None
    }

    pub async fn run(mut self, mut shutdown: watch::Receiver<bool>) {
        let mut status = self.modem.watch();
        loop {
            if *shutdown.borrow() {
                return;
            }
            let mut pause = Duration::from_secs(2);
            if self.modem.is_ready() {
                match self.step().await {
                    Ok(Some(wait)) => pause = wait,
                    Ok(None) => continue, // a message was processed; look for more
                    Err(e) => {
                        tracing::error!(modem = %self.modem.id, error = %e, "outbound queue error");
                        pause = Duration::from_secs(5);
                    }
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(pause) => {}
                _ = self.svc.wake.notified() => {}
                _ = status.changed() => {}
                _ = shutdown.changed() => {}
            }
        }
    }

    /// Process at most one message. `Ok(Some(wait))` means nothing was sent
    /// and the worker should idle for `wait`.
    async fn step(&mut self) -> Result<Option<Duration>, crate::database::DbError> {
        let cfg = self.svc.cfg.load_full();
        let id = self.modem.id.clone();
        if !cfg.sms.enabled || !cfg.sms.send_enabled {
            return Ok(Some(Duration::from_secs(5)));
        }
        if let Some(wait) = self.rate_wait(cfg.sms.outbound_rate_limit_per_minute) {
            return Ok(Some(wait.min(Duration::from_secs(5))));
        }
        // A SIM that has used up its quota leaves unpinned messages to the
        // other modems instead of claiming and parking them.
        let quota_left = self.svc.quota_exhausted(&cfg, &id, 1).await?.is_none();
        let take_unassigned =
            quota_left && (cfg.sms.default_modem.is_empty() || cfg.sms.default_modem == id);
        let Some(item) = self.svc.db.claim_next(&id, take_unassigned).await? else {
            return Ok(Some(Duration::from_secs(2)));
        };
        self.send(item).await?;
        Ok(None)
    }

    async fn send(&mut self, item: QueueItem) -> Result<(), crate::database::DbError> {
        let cfg = self.svc.cfg.load_full();
        let db = &self.svc.db;
        let modem_id = self.modem.id.clone();
        let mid = item.message_id.clone();
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
        let parts = db.unsent_parts(&mid).await?;
        // Spending protection and throttling. Deferred messages are released
        // without consuming an attempt.
        if let Some(reason) = self
            .svc
            .quota_exhausted(&cfg, &modem_id, parts.len() as i64)
            .await?
        {
            tracing::warn!(modem = %modem_id, message_id = %mid, "{reason}; message deferred");
            db.add_history(&mid, "queued", Some(&format!("deferred: {reason}")))
                .await?;
            // Pinned messages wait for the quota window; unpinned ones are
            // offered to the other modems again shortly.
            let delay = if item.modem_id.is_some() { 600 } else { 20 };
            return db.release(&mid, Some(utils::ts_after(delay))).await;
        }
        if cfg.sms.per_destination_interval_seconds > 0 {
            if let Some(last) = message
                .recipient
                .as_deref()
                .map(|r| db.last_submission_to(r))
            {
                if let Some(last) = last.await?.and_then(|t| utils::parse_ts(&t)) {
                    let next = last
                        + chrono::Duration::seconds(
                            cfg.sms.per_destination_interval_seconds as i64,
                        );
                    if next > chrono::Utc::now() {
                        return db.release(&mid, Some(utils::ts(next))).await;
                    }
                }
            }
        }
        let sim = cfg.sim_for_modem(&modem_id);
        let sim_id = self
            .modem
            .status()
            .sim
            .profile
            .or(sim.map(|s| s.id.clone()));
        let own_number = sim
            .map(|s| s.mobile_number.clone())
            .filter(|n| !n.is_empty());
        db.assign_modem(&mid, &modem_id, sim_id.as_deref(), own_number.as_deref())
            .await?;

        let mut first_ref = None;
        for part in &parts {
            let (Some(pdu), Some(len)) = (part.pdu.clone(), part.tpdu_length) else {
                db.update_part(&part.id, "failed", None, Some("part has no PDU"))
                    .await?;
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
            // Persist "sending" first: if the daemon dies now, recovery knows
            // the outcome of this part is uncertain.
            db.update_part(&part.id, "sending", None, None).await?;
            match self.modem.send_pdu(len as usize, pdu).await {
                Ok(mr) => {
                    db.update_part(&part.id, "submitted", Some(mr as i64), None)
                        .await?;
                    first_ref.get_or_insert(mr as i64);
                    self.window.push_back(Instant::now());
                    self.svc.metrics.inc(
                        "smsgw_message_parts_submitted_total",
                        &[("modem", &modem_id)],
                    );
                }
                Err(SendError::NotReady(why)) => {
                    db.update_part(&part.id, "queued", None, None).await?;
                    db.add_history(&mid, "queued", Some(&format!("deferred: {why}")))
                        .await?;
                    return db.release(&mid, Some(utils::ts_after(10))).await;
                }
                Err(SendError::Rejected(why)) => {
                    db.update_part(&part.id, "failed", None, Some(&why)).await?;
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
                    tracing::warn!(target: "messages", message_id = %mid, modem = %modem_id, attempt, retry_in = delay, error = %why, "submission rejected; will retry");
                    return db
                        .set_message_state(
                            &mid,
                            "retrying",
                            Some(&why),
                            true,
                            Some(utils::ts_after(delay as i64)),
                        )
                        .await;
                }
                Err(SendError::Ambiguous(why)) => {
                    // The modem may have transmitted the message. Retrying
                    // could send (and bill) it twice, so this needs a human.
                    db.update_part(&part.id, "unknown", None, Some(&why))
                        .await?;
                    db.set_message_state(
                        &mid,
                        "submission_unknown",
                        Some(&format!("{why}; not retried automatically")),
                        true,
                        None,
                    )
                    .await?;
                    return self.failed_event(&mid).await;
                }
            }
        }
        if let Some(r) = first_ref {
            db.set_modem_ref(&mid, r).await?;
        }
        db.set_message_state(&mid, "submitted", None, false, None)
            .await?;
        self.svc
            .metrics
            .inc("smsgw_messages_submitted_total", &[("modem", &modem_id)]);
        if let Some(m) = db.get_message(&mid).await? {
            self.svc.log_message("submitted", &m, &cfg);
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

    async fn failed_event(&self, mid: &str) -> Result<(), crate::database::DbError> {
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
