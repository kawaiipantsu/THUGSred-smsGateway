//! Gateway runtime: wires configuration, database, modems, queue workers,
//! integrations and housekeeping together and owns their lifecycle.

use crate::calls::CallManager;
use crate::config::{self, Config, ConfigError, LoadOptions, SharedConfig};
use crate::database::Db;
use crate::events::Events;
use crate::integrations::worker::Dispatcher;
use crate::metrics::{self, Metrics};
use crate::modem::profiles::ProfileSet;
use crate::modem::transport::Connector;
use crate::modem::{manager, CallSignal, Modem, ModemDeps};
use crate::sms::{sender::Sender, SmsService};
use crate::utils;
use serde::Serialize;
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GIT_COMMIT: &str = env!("SMSGW_GIT_COMMIT");
pub const BUILD_DATE: &str = env!("SMSGW_BUILD_DATE");
pub const BUILD_TARGET: &str = env!("SMSGW_TARGET");

pub fn version_info() -> serde_json::Value {
    json!({
        "name": "THUGSred-smsGateway",
        "version": VERSION,
        "commit": GIT_COMMIT,
        "build_date": BUILD_DATE,
        "target": BUILD_TARGET,
        "homepage": "https://thugs.red",
    })
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ConfigStatus {
    pub valid: bool,
    pub loaded_at: String,
    pub last_reload_at: Option<String>,
    /// Error of the last failed reload; the previous configuration stays active.
    pub last_reload_error: Option<String>,
    pub warnings: Vec<String>,
    /// Changed settings that need a restart to take effect.
    pub restart_required: Vec<String>,
}

type LevelHook = Box<dyn Fn(&str) + Send + Sync>;

pub struct Gateway {
    pub cfg: SharedConfig,
    pub db: Db,
    pub events: Events,
    pub metrics: Arc<Metrics>,
    pub sms: SmsService,
    pub modems: Vec<Modem>,
    pub profiles: Arc<ProfileSet>,
    pub integration_sender: crate::integrations::Sender,
    pub calls_tx: mpsc::Sender<CallSignal>,
    pub started_at: String,
    pub config_status: Mutex<ConfigStatus>,
    load_options: LoadOptions,
    shutdown_tx: watch::Sender<bool>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    level_hook: Mutex<Option<LevelHook>>,
}

impl Gateway {
    /// Start the gateway. `connector` supplies the stream for each modem id,
    /// which lets tests and the simulator replace the serial port.
    pub async fn start(
        cfg: Config,
        load_options: LoadOptions,
        connector: impl Fn(&str) -> Arc<dyn Connector>,
    ) -> anyhow::Result<Arc<Gateway>> {
        let db = Db::connect(&cfg.database).await?;
        if cfg.database.auto_migrate {
            let applied = db.migrate().await?;
            if !applied.is_empty() {
                tracing::info!(versions = ?applied, "database migrations applied");
            }
        } else {
            let have = db.schema_version().await.unwrap_or(0);
            anyhow::ensure!(
                have == db.latest_version(),
                "database schema is at version {have} but version {} is required; run `thugsred-smsgateway db migrate` or enable database.auto_migrate",
                db.latest_version()
            );
        }
        let (profiles, profile_warnings) = ProfileSet::load(&cfg.paths.modem_profiles);
        for w in &profile_warnings {
            tracing::warn!("modem profile: {w}");
        }
        let profiles = Arc::new(profiles);
        let status = ConfigStatus {
            valid: true,
            loaded_at: utils::now(),
            warnings: cfg.warnings.clone(),
            ..Default::default()
        };
        let shared = config::shared(cfg);
        let metrics = Arc::new(Metrics::new());
        let events = Events::new(db.clone(), shared.clone(), metrics.clone());
        let sms = SmsService::new(shared.clone(), db.clone(), events.clone(), metrics.clone());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (calls_tx, calls_rx) = mpsc::channel::<CallSignal>(64);
        let mut tasks = Vec::new();

        // Messages that were mid-flight when the previous run ended.
        match db.recover_processing().await {
            Ok((requeued, unknown)) if requeued + unknown > 0 => {
                tracing::warn!(
                    requeued,
                    submission_unknown = unknown,
                    "recovered outbound queue after restart"
                )
            }
            Err(e) => tracing::error!(error = %e, "outbound queue recovery failed"),
            _ => {}
        }

        let cfg_now = shared.load_full();
        let mut modems = Vec::new();
        for mc in &cfg_now.modems {
            let deps = ModemDeps {
                cfg: shared.clone(),
                db: db.clone(),
                events: events.clone(),
                metrics: metrics.clone(),
                sink: Arc::new(sms.clone()),
                calls: calls_tx.clone(),
                connector: connector(&mc.id),
                profiles: profiles.clone(),
            };
            let (modem, handle) = manager::spawn(&mc.id, deps, shutdown_rx.clone());
            tasks.push(handle);
            tasks.push(tokio::spawn(
                Sender::new(sms.clone(), modem.clone()).run(shutdown_rx.clone()),
            ));
            modems.push(modem);
        }
        tasks.push(tokio::spawn(
            crate::providers::ProviderWorker::new(sms.clone()).run(shutdown_rx.clone()),
        ));
        let calls = CallManager::new(
            shared.clone(),
            db.clone(),
            events.clone(),
            metrics.clone(),
            modems.clone(),
            calls_tx.clone(),
        );
        tasks.push(tokio::spawn(calls.run(calls_rx, shutdown_rx.clone())));
        let dispatcher = Dispatcher::new(
            db.clone(),
            shared.clone(),
            metrics.clone(),
            events.wake_handle(),
        );
        tasks.push(tokio::spawn(dispatcher.run(shutdown_rx.clone())));

        let gw = Arc::new(Gateway {
            cfg: shared,
            db,
            events,
            metrics,
            sms,
            modems,
            profiles,
            integration_sender: crate::integrations::Sender::new(),
            calls_tx,
            started_at: utils::now(),
            config_status: Mutex::new(status),
            load_options,
            shutdown_tx,
            tasks: Mutex::new(tasks),
            level_hook: Mutex::new(None),
        });
        let hk = tokio::spawn(housekeeping(gw.clone(), shutdown_rx));
        gw.tasks.lock().unwrap_or_else(|e| e.into_inner()).push(hk);
        gw.events
            .emit(
                "gateway.started",
                None,
                json!({"version": VERSION, "commit": GIT_COMMIT, "modems": gw.modems.len()}),
            )
            .await;
        Ok(gw)
    }

    /// Register the callback that applies a new log level on reload.
    pub fn set_level_hook(&self, hook: impl Fn(&str) + Send + Sync + 'static) {
        *self.level_hook.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(hook));
    }

    /// Look up a modem; `None` or an empty id selects the first modem.
    pub fn modem(&self, id: Option<&str>) -> Option<&Modem> {
        match id.filter(|s| !s.is_empty()) {
            Some(id) => self.modems.iter().find(|m| m.id == id),
            None => self.modems.first(),
        }
    }

    pub fn shutdown_signal(&self) -> watch::Receiver<bool> {
        self.shutdown_tx.subscribe()
    }

    pub fn config_status(&self) -> ConfigStatus {
        self.config_status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Reload configuration from disk. An invalid configuration is rejected
    /// as a whole and the running configuration stays in effect.
    pub async fn reload(&self) -> Result<Vec<String>, ConfigError> {
        let result = config::load(&self.load_options);
        let now = utils::now();
        match result {
            Ok(new) => {
                let old = self.cfg.load_full();
                let restart: Vec<String> = old
                    .restart_required(&new)
                    .into_iter()
                    .map(str::to_string)
                    .collect();
                if let Some(hook) = self
                    .level_hook
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_ref()
                {
                    hook(&new.general.log_level);
                }
                {
                    let mut st = self.config_status.lock().unwrap_or_else(|e| e.into_inner());
                    st.last_reload_at = Some(now);
                    st.last_reload_error = None;
                    st.warnings = new.warnings.clone();
                    st.restart_required = restart.clone();
                }
                // Atomic swap: readers see either the old or the new configuration.
                self.cfg.store(Arc::new(new));
                tracing::info!(target: "audit", restart_required = ?restart, "configuration reloaded");
                let _ = self
                    .db
                    .audit(
                        Some("system"),
                        "config.reload",
                        None,
                        Some(&format!("restart required for: {restart:?}")),
                        None,
                    )
                    .await;
                self.events
                    .emit(
                        "gateway.config_reloaded",
                        None,
                        json!({"restart_required": restart}),
                    )
                    .await;
                self.sms.wake.notify_waiters();
                Ok(restart)
            }
            Err(e) => {
                {
                    let mut st = self.config_status.lock().unwrap_or_else(|e| e.into_inner());
                    st.last_reload_at = Some(now);
                    st.last_reload_error = Some(e.to_string());
                }
                tracing::error!(target: "audit", error = %e, "configuration reload rejected; keeping the previous configuration");
                let _ = self
                    .db
                    .audit(
                        Some("system"),
                        "config.reload_failed",
                        None,
                        Some(&e.to_string()),
                        None,
                    )
                    .await;
                self.events.emit("gateway.error", None, json!({"error": "configuration reload rejected; previous configuration kept", "details": e.errors})).await;
                Err(e)
            }
        }
    }

    /// Stop all tasks, waiting up to the configured shutdown timeout.
    pub async fn shutdown(&self) {
        let timeout = Duration::from_secs(self.cfg.load().general.shutdown_timeout_seconds.max(1));
        self.events
            .emit("gateway.stopped", None, json!({"version": VERSION}))
            .await;
        // Let the dispatcher pick up the final event before stopping.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = self.shutdown_tx.send(true);
        let tasks: Vec<JoinHandle<()>> =
            std::mem::take(&mut *self.tasks.lock().unwrap_or_else(|e| e.into_inner()));
        let all = async {
            for t in tasks {
                let _ = t.await;
            }
        };
        if tokio::time::timeout(timeout, all).await.is_err() {
            tracing::warn!("shutdown timeout reached; some tasks were still running");
        }
        self.db.close().await;
    }
}

fn cutoff(days: u32) -> Option<String> {
    (days > 0).then(|| utils::ts_after(-(days as i64) * 86400))
}

/// Run retention once. Returns a per-table report.
pub async fn run_retention(gw: &Gateway) -> Result<serde_json::Value, crate::database::DbError> {
    let cfg = gw.cfg.load_full();
    let r = &cfg.retention;
    let (report, files) = gw
        .db
        .apply_retention(
            cutoff(r.message_days),
            cutoff(r.recording_days),
            cutoff(r.attachment_days),
            cutoff(r.integration_event_days),
            cutoff(r.audit_days),
        )
        .await?;
    // Only remove files inside the configured storage directories.
    for f in files {
        let p = std::path::Path::new(&f);
        if p.starts_with(&cfg.paths.recordings) || p.starts_with(&cfg.paths.attachments) {
            let _ = tokio::fs::remove_file(p).await;
        }
    }
    Ok(report)
}

async fn housekeeping(gw: Arc<Gateway>, mut shutdown: watch::Receiver<bool>) {
    let mut fast = tokio::time::interval(Duration::from_secs(15));
    let mut last_cleanup = tokio::time::Instant::now();
    let mut disk_warned = false;
    loop {
        tokio::select! {
            _ = shutdown.changed() => return,
            _ = fast.tick() => {}
        }
        let cfg = gw.cfg.load_full();
        match gw.db.expire_due().await {
            Ok(ids) => {
                for id in ids {
                    if let Ok(Some(m)) = gw.db.get_message(&id).await {
                        gw.sms.log_message("expired", &m, &cfg);
                        gw.events
                            .emit("sms.failed", Some(&id), crate::events::message_data(&m))
                            .await;
                    }
                }
            }
            Err(e) => tracing::error!(error = %e, "queue expiry check failed"),
        }
        if let Err(e) = gw.sms.flush_stale_multipart().await {
            tracing::error!(error = %e, "multipart cleanup failed");
        }
        let db_up = gw.db.ping().await;
        gw.metrics
            .set("smsgw_database_up", &[], if db_up { 1.0 } else { 0.0 });
        if let Ok(depth) = gw.db.queue_depth().await {
            gw.metrics.set("smsgw_queue_depth", &[], depth as f64);
        }
        if let Some((free, total)) = metrics::disk_usage(&cfg.paths.data) {
            gw.metrics.set("smsgw_disk_free_bytes", &[], free as f64);
            gw.metrics.set("smsgw_disk_total_bytes", &[], total as f64);
            let low = free < 200 * 1024 * 1024 || (total > 0 && free * 50 < total);
            if low && !disk_warned {
                gw.events.emit("gateway.error", None, json!({"error": format!("low disk space on {}: {} MiB free", cfg.paths.data, free / 1048576)})).await;
            }
            disk_warned = low;
        }
        if last_cleanup.elapsed() >= Duration::from_secs(cfg.retention.cleanup_interval_seconds) {
            last_cleanup = tokio::time::Instant::now();
            match run_retention(&gw).await {
                Ok(report) => tracing::debug!(report = %report, "retention applied"),
                Err(e) => tracing::error!(error = %e, "retention failed"),
            }
        }
    }
}
