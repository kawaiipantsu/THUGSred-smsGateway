//! Call event tracking, optional automatic answering and optional recording.
//!
//! Recording captures audio from a host ALSA device that must be wired to the
//! modem's audio output (sound card, USB audio or PCM bridge). The AT serial
//! line carries no audio, so without such a device recording is reported as
//! unavailable; the gateway never fabricates a recording.

use crate::config::SharedConfig;
use crate::database::Db;
use crate::events::Events;
use crate::metrics::Metrics;
use crate::modem::{CallSignal, CallSignalKind, Modem};
use crate::utils;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};

struct ActiveCall {
    id: String,
    direction: &'static str,
    number: Option<String>,
    started: Instant,
    last_ring: Instant,
    announced: bool,
    answered_at: Option<Instant>,
    recording_status: &'static str,
    recording_id: Option<String>,
}

pub struct CallManager {
    pub cfg: SharedConfig,
    pub db: Db,
    pub events: Events,
    pub metrics: Arc<Metrics>,
    pub modems: Vec<Modem>,
    active: HashMap<String, ActiveCall>,
    internal_tx: mpsc::Sender<CallSignal>,
}

/// Outcome of a recording attempt.
#[derive(Debug, PartialEq, Eq)]
pub enum Recording {
    Saved { path: String, bytes: u64 },
    Unavailable(String),
    Failed(String),
}

/// Record `seconds` of audio from an ALSA capture device into `path`.
pub async fn record(
    device: &str,
    backend: &str,
    seconds: u64,
    path: &std::path::Path,
) -> Recording {
    if backend == "none" {
        return Recording::Unavailable(
            "audio backend is disabled (calls.audio_backend = \"none\")".into(),
        );
    }
    if device.is_empty() {
        return Recording::Unavailable("no capture device configured (calls.audio_device)".into());
    }
    if let Some(dir) = path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(dir).await {
            return Recording::Failed(format!("cannot create recordings directory: {e}"));
        }
    }
    let run = tokio::process::Command::new("arecord")
        .args([
            "-q", "-D", device, "-f", "S16_LE", "-r", "8000", "-c", "1", "-t", "wav", "-d",
        ])
        .arg(seconds.to_string())
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .output();
    let out = match tokio::time::timeout(Duration::from_secs(seconds + 10), run).await {
        Err(_) => {
            let _ = tokio::fs::remove_file(path).await;
            return Recording::Failed("recorder did not finish in time".into());
        }
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Recording::Unavailable("arecord (alsa-utils) is not installed".into());
        }
        Ok(Err(e)) => return Recording::Failed(format!("cannot start recorder: {e}")),
        Ok(Ok(o)) => o,
    };
    let size = tokio::fs::metadata(path)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    // A WAV header alone is 44 bytes; anything that small holds no audio.
    if !out.status.success() || size <= 1024 {
        let _ = tokio::fs::remove_file(path).await;
        let err = String::from_utf8_lossy(&out.stderr)
            .lines()
            .next()
            .unwrap_or("no audio captured")
            .trim()
            .to_string();
        return Recording::Failed(if err.is_empty() {
            "no audio captured".into()
        } else {
            err
        });
    }
    Recording::Saved {
        path: path.display().to_string(),
        bytes: size,
    }
}

impl CallManager {
    pub fn new(
        cfg: SharedConfig,
        db: Db,
        events: Events,
        metrics: Arc<Metrics>,
        modems: Vec<Modem>,
        internal_tx: mpsc::Sender<CallSignal>,
    ) -> Self {
        Self {
            cfg,
            db,
            events,
            metrics,
            modems,
            active: HashMap::new(),
            internal_tx,
        }
    }

    fn call_data(
        &self,
        modem_id: &str,
        c: &ActiveCall,
        extra: serde_json::Value,
    ) -> serde_json::Value {
        let mut call = json!({
            "id": c.id,
            "modem_id": modem_id,
            "direction": c.direction,
            "number": c.number,
            "answered": c.answered_at.is_some(),
            "recording_status": c.recording_status,
        });
        if let (Some(a), Some(b)) = (call.as_object_mut(), extra.as_object()) {
            a.extend(b.clone());
        }
        json!({"call": call, "modem_id": modem_id})
    }

    pub async fn run(
        mut self,
        mut rx: mpsc::Receiver<CallSignal>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        if let Ok(n) = self.db.close_open_calls().await {
            if n > 0 {
                tracing::info!(count = n, "closed call records left open by a previous run");
            }
        }
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                _ = shutdown.changed() => break,
                Some(sig) = rx.recv() => self.on_signal(sig).await,
                _ = tick.tick() => self.on_tick().await,
            }
        }
        let open: Vec<String> = self.active.keys().cloned().collect();
        for m in open {
            self.finish(&m, "gateway_shutdown").await;
        }
    }

    async fn on_signal(&mut self, sig: CallSignal) {
        let cfg = self.cfg.load_full();
        if !cfg.calls.enabled {
            return;
        }
        let modem_id = sig.modem_id.clone();
        match sig.kind {
            CallSignalKind::Ring => {
                if !cfg.calls.log_incoming {
                    return;
                }
                if let Some(c) = self.active.get_mut(&modem_id) {
                    c.last_ring = Instant::now();
                    return;
                }
                let id = utils::new_id();
                if let Err(e) = self
                    .db
                    .insert_call(&id, &modem_id, "incoming", None, "ringing")
                    .await
                {
                    tracing::error!(error = %e, "could not store call record");
                }
                self.metrics.inc(
                    "smsgw_calls_total",
                    &[("modem", &modem_id), ("direction", "incoming")],
                );
                self.active.insert(
                    modem_id,
                    ActiveCall {
                        id,
                        direction: "incoming",
                        number: None,
                        started: Instant::now(),
                        last_ring: Instant::now(),
                        announced: false,
                        answered_at: None,
                        recording_status: "disabled",
                        recording_id: None,
                    },
                );
            }
            CallSignalKind::CallerId(number) => {
                let Some(c) = self.active.get_mut(&modem_id) else {
                    return;
                };
                c.last_ring = Instant::now();
                if c.number.is_none() && !number.is_empty() {
                    c.number = Some(number.clone());
                    let _ = self.db.set_call_number(&c.id, &number).await;
                }
                if !c.announced {
                    self.announce(&modem_id).await;
                }
            }
            CallSignalKind::Dialed(number) => {
                if !cfg.calls.log_outgoing || self.active.contains_key(&modem_id) {
                    return;
                }
                let id = utils::new_id();
                let _ = self
                    .db
                    .insert_call(&id, &modem_id, "outgoing", Some(&number), "dialing")
                    .await;
                self.metrics.inc(
                    "smsgw_calls_total",
                    &[("modem", &modem_id), ("direction", "outgoing")],
                );
                self.active.insert(
                    modem_id,
                    ActiveCall {
                        id,
                        direction: "outgoing",
                        number: Some(number),
                        started: Instant::now(),
                        last_ring: Instant::now(),
                        announced: true,
                        answered_at: None,
                        recording_status: "disabled",
                        recording_id: None,
                    },
                );
            }
            CallSignalKind::Ended(reason) => self.finish(&modem_id, &reason).await,
            CallSignalKind::Lost => self.finish(&modem_id, "modem_disconnected").await,
        }
    }

    async fn announce(&mut self, modem_id: &str) {
        let cfg = self.cfg.load_full();
        let Some(c) = self.active.get_mut(modem_id) else {
            return;
        };
        c.announced = true;
        let call_id = c.id.clone();
        let number_log = c.number.as_deref().map(|n| {
            if cfg.is_anonymous(n) {
                cfg.anonymous.label.clone()
            } else if cfg.logging.redact_sensitive_values {
                utils::mask_number(n)
            } else {
                n.to_string()
            }
        });
        tracing::info!(target: "messages", call_id = %call_id, modem = %modem_id, number = ?number_log, "incoming call");
        let data = self.call_data(modem_id, &self.active[modem_id], json!({}));
        self.events
            .emit("call.incoming", Some(&call_id), data)
            .await;
        if !cfg.calls.auto_answer {
            return;
        }
        let Some(modem) = self.modems.iter().find(|m| m.id == modem_id).cloned() else {
            return;
        };
        if let Err(e) = modem.answer().await {
            tracing::warn!(modem = %modem_id, error = %e, "automatic answer failed");
            return;
        }
        let _ = self.db.set_call_answered(&call_id).await;
        if let Some(c) = self.active.get_mut(modem_id) {
            c.answered_at = Some(Instant::now());
            c.recording_status = if cfg.calls.recording_enabled {
                "recording"
            } else {
                "disabled"
            };
        }
        // Record (when enabled), then hang up. Runs detached so that call
        // signals keep being processed meanwhile.
        let (db, events, tx) = (
            self.db.clone(),
            self.events.clone(),
            self.internal_tx.clone(),
        );
        let modem_id = modem_id.to_string();
        let number = self.active.get(&modem_id).and_then(|c| c.number.clone());
        tokio::spawn(async move {
            let secs = cfg.calls.recording_duration_seconds;
            let mut outcome = None;
            if cfg.calls.recording_enabled {
                let rec_id = utils::new_id();
                // The file name carries no caller information.
                let path = std::path::Path::new(&cfg.paths.recordings)
                    .join(format!("call-{rec_id}.{}", cfg.calls.recording_format));
                match record(
                    &cfg.calls.audio_device,
                    &cfg.calls.audio_backend,
                    secs,
                    &path,
                )
                .await
                {
                    Recording::Saved { path, bytes } => {
                        let _ = db
                            .insert_recording(
                                &rec_id,
                                &call_id,
                                &path,
                                &cfg.calls.recording_format,
                                secs as i64,
                                bytes as i64,
                            )
                            .await;
                        events
                            .emit("call.recorded", Some(&call_id), json!({"call": {"id": call_id, "modem_id": modem_id, "number": number, "recording_id": rec_id, "recording_seconds": secs, "recording_bytes": bytes}, "modem_id": modem_id}))
                            .await;
                        outcome = Some(("recorded", Some(rec_id)));
                    }
                    Recording::Unavailable(why) => {
                        tracing::warn!(modem = %modem_id, "call recording unavailable: {why}");
                        outcome = Some(("unavailable", None));
                        tokio::time::sleep(Duration::from_secs(secs)).await;
                    }
                    Recording::Failed(why) => {
                        tracing::warn!(modem = %modem_id, "call recording failed: {why}");
                        outcome = Some(("failed", None));
                    }
                }
            } else {
                tokio::time::sleep(Duration::from_secs(secs)).await;
            }
            if let Some((status, rec)) = outcome {
                let _ = db
                    .kv_set(
                        &format!("call_recording:{call_id}"),
                        &format!("{status}:{}", rec.unwrap_or_default()),
                    )
                    .await;
            }
            if cfg.calls.hangup_after_recording {
                let _ = modem.hangup().await;
                let _ = tx
                    .send(CallSignal {
                        modem_id,
                        kind: CallSignalKind::Ended("gateway_hangup".into()),
                    })
                    .await;
            }
        });
    }

    async fn on_tick(&mut self) {
        let ring_timeout = Duration::from_secs(self.cfg.load().calls.ring_timeout_seconds.max(2));
        let mut announce = Vec::new();
        let mut missed = Vec::new();
        for (modem, c) in &self.active {
            if c.direction != "incoming" {
                continue;
            }
            if !c.announced && c.started.elapsed() > Duration::from_millis(1500) {
                announce.push(modem.clone()); // no caller id was presented
            } else if c.answered_at.is_none() && c.last_ring.elapsed() > ring_timeout {
                missed.push(modem.clone());
            }
        }
        for m in announce {
            self.announce(&m).await;
        }
        for m in missed {
            self.finish(&m, "ring_timeout").await;
        }
    }

    async fn finish(&mut self, modem_id: &str, reason: &str) {
        let Some(mut c) = self.active.remove(modem_id) else {
            return;
        };
        if !c.announced && c.direction == "incoming" {
            // Ended before it was announced: still report the call.
            self.active.insert(modem_id.to_string(), c);
            self.announce(modem_id).await;
            let Some(again) = self.active.remove(modem_id) else {
                return;
            };
            c = again;
        }
        if let Ok(Some(v)) = self.db.kv_get(&format!("call_recording:{}", c.id)).await {
            if let Some((status, rec)) = v.split_once(':') {
                c.recording_status = match status {
                    "recorded" => "recorded",
                    "unavailable" => "unavailable",
                    _ => "failed",
                };
                c.recording_id = Some(rec.to_string()).filter(|r| !r.is_empty());
            }
        } else if c.recording_status == "recording" {
            c.recording_status = "failed";
        }
        let missed = c.direction == "incoming" && c.answered_at.is_none();
        let duration = c.answered_at.map(|a| a.elapsed().as_secs() as i64);
        let state = if missed { "missed" } else { "ended" };
        if let Err(e) = self
            .db
            .finish_call(
                &c.id,
                state,
                missed,
                duration,
                Some(reason),
                c.recording_status,
                c.recording_id.as_deref(),
            )
            .await
        {
            tracing::error!(error = %e, "could not update call record");
        }
        tracing::info!(target: "messages", call_id = %c.id, modem = %modem_id, state, reason, duration_seconds = ?duration, "call finished");
        let data = self.call_data(modem_id, &c, json!({"state": state, "termination_reason": reason, "duration_seconds": duration, "recording_id": c.recording_id}));
        self.events
            .emit(
                if missed { "call.missed" } else { "call.ended" },
                Some(&c.id),
                data,
            )
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn recording_is_never_faked() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.wav");
        assert!(matches!(
            record("", "auto", 1, &p).await,
            Recording::Unavailable(_)
        ));
        assert!(matches!(
            record("hw:0,0", "none", 1, &p).await,
            Recording::Unavailable(_)
        ));
        // A device that does not exist must not yield a "successful" recording.
        let r = record("thugsred_no_such_device", "alsa", 1, &p).await;
        assert!(
            matches!(r, Recording::Failed(_) | Recording::Unavailable(_)),
            "{r:?}"
        );
        assert!(!p.exists(), "no file may be left behind");
    }
}
