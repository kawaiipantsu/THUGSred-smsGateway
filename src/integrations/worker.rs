//! Persistent integration delivery worker with retries, exponential backoff,
//! per-integration rate limits and dead-lettering.

use super::{Outcome, Sender};
use crate::config::SharedConfig;
use crate::database::{Db, Delivery};
use crate::metrics::Metrics;
use crate::utils;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{watch, Notify, Semaphore};

#[derive(Clone)]
pub struct Dispatcher {
    pub db: Db,
    pub cfg: SharedConfig,
    pub sender: Sender,
    pub metrics: Arc<Metrics>,
    wake: Arc<Notify>,
    windows: Arc<Mutex<HashMap<String, VecDeque<Instant>>>>,
}

impl Dispatcher {
    pub fn new(db: Db, cfg: SharedConfig, metrics: Arc<Metrics>, wake: Arc<Notify>) -> Self {
        Self {
            db,
            cfg,
            sender: Sender::new(),
            metrics,
            wake,
            windows: Default::default(),
        }
    }

    /// Returns the seconds to wait if the integration's rate limit is exhausted.
    fn rate_limited(&self, name: &str, per_minute: u32) -> Option<u64> {
        if per_minute == 0 {
            return None;
        }
        let mut w = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        let q = w.entry(name.to_string()).or_default();
        let now = Instant::now();
        while q
            .front()
            .map(|t| now.duration_since(*t) >= Duration::from_secs(60))
            .unwrap_or(false)
        {
            q.pop_front();
        }
        if q.len() >= per_minute as usize {
            let wait = 60u64.saturating_sub(
                q.front()
                    .map(|t| now.duration_since(*t).as_secs())
                    .unwrap_or(0),
            );
            return Some(wait.max(1));
        }
        q.push_back(now);
        None
    }

    /// Process one claimed delivery to completion.
    pub async fn process(&self, d: Delivery) {
        let cfg = self.cfg.load_full();
        let Some(def) = cfg
            .integration_def(&d.integration)
            .filter(|i| i.enabled && cfg.integrations.enabled)
        else {
            let _ = self
                .db
                .finish_delivery(
                    &d,
                    "dead",
                    false,
                    None,
                    Some("integration is disabled or no longer configured"),
                    0,
                    None,
                )
                .await;
            return;
        };
        if let Some(wait) = self.rate_limited(&d.integration, def.rate_limit_per_minute) {
            let _ = self
                .db
                .finish_delivery(
                    &d,
                    "pending",
                    false,
                    None,
                    None,
                    0,
                    Some(utils::ts_after(wait as i64)),
                )
                .await;
            return;
        }
        let ev = match self.db.get_event(&d.event_id).await {
            Ok(Some(ev)) => ev,
            _ => {
                let _ = self
                    .db
                    .finish_delivery(
                        &d,
                        "dead",
                        false,
                        None,
                        Some("event record is missing"),
                        0,
                        None,
                    )
                    .await;
                return;
            }
        };
        let outcome: Outcome = self.sender.deliver(def, &ev, &cfg).await;
        self.metrics.inc(
            "smsgw_integration_attempts_total",
            &[("integration", &d.integration)],
        );
        let attempt = d.attempts + 1;
        let max_attempts = if def.max_attempts > 0 {
            def.max_attempts
        } else {
            cfg.integrations.max_attempts
        } as i64;
        let initial = if def.retry_initial_seconds > 0 {
            def.retry_initial_seconds
        } else {
            cfg.integrations.retry_initial_seconds
        };
        let max = if def.retry_max_seconds > 0 {
            def.retry_max_seconds
        } else {
            cfg.integrations.retry_max_seconds
        };
        let (status, next) = if outcome.success {
            ("delivered", None)
        } else if outcome.retryable && attempt < max_attempts {
            (
                "retrying",
                Some(utils::ts_after(
                    utils::backoff_seconds(initial, max, attempt as u32) as i64,
                )),
            )
        } else {
            ("dead", None)
        };
        if outcome.success {
            tracing::info!(target: "integrations", integration = %d.integration, event = %ev.event_type, event_id = %ev.id, attempt, status_code = ?outcome.status_code, duration_ms = outcome.duration_ms, "delivered");
        } else {
            self.metrics.inc(
                "smsgw_integration_failures_total",
                &[("integration", &d.integration)],
            );
            tracing::warn!(target: "integrations", integration = %d.integration, event = %ev.event_type, event_id = %ev.id, attempt, status_code = ?outcome.status_code, error = ?outcome.error, next = status, "delivery failed");
        }
        if let Err(e) = self
            .db
            .finish_delivery(
                &d,
                status,
                true,
                outcome.status_code.map(i64::from),
                outcome.error.as_deref(),
                outcome.duration_ms as i64,
                next,
            )
            .await
        {
            tracing::error!(error = %e, "failed to record integration delivery outcome");
        }
        if let Some(id) = ev
            .correlation_id
            .as_deref()
            .filter(|_| ev.event_type.starts_with("sms.") || ev.event_type.starts_with("mms."))
        {
            let s = match status {
                "delivered" => "delivered",
                "dead" => "failed",
                _ => "pending",
            };
            let _ = self.db.set_integration_status(id, s).await;
        }
    }

    /// Run until shutdown. Up to `worker_count` deliveries run concurrently.
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        let workers = self.cfg.load().integrations.worker_count.max(1);
        let sem = Arc::new(Semaphore::new(workers));
        let discard = !self.cfg.load().integrations.persistent_queue;
        match self.db.recover_deliveries(discard).await {
            Ok(n) if n > 0 => tracing::info!(
                count = n,
                discarded = discard,
                "recovered integration deliveries from previous run"
            ),
            Err(e) => tracing::error!(error = %e, "integration delivery recovery failed"),
            _ => {}
        }
        loop {
            if *shutdown.borrow() {
                break;
            }
            let free = sem.available_permits();
            let mut claimed = 0;
            if free > 0 {
                match self.db.claim_deliveries(free as i64).await {
                    Ok(list) => {
                        claimed = list.len();
                        for d in list {
                            let Ok(permit) = sem.clone().acquire_owned().await else {
                                return;
                            };
                            let me = self.clone();
                            tokio::spawn(async move {
                                me.process(d).await;
                                drop(permit);
                                me.wake.notify_waiters();
                            });
                        }
                    }
                    Err(e) => tracing::error!(error = %e, "claiming integration deliveries failed"),
                }
            }
            if claimed > 0 && sem.available_permits() > 0 {
                continue;
            }
            tokio::select! {
                _ = self.wake.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                _ = shutdown.changed() => {}
            }
        }
        // Give in-flight deliveries a moment to finish recording their outcome.
        let _ =
            tokio::time::timeout(Duration::from_secs(10), sem.acquire_many(workers as u32)).await;
    }
}
