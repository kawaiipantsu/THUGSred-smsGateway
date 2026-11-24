//! Per-modem supervisor: connection lifecycle, initialisation, SIM handling,
//! registration monitoring, inbound message retrieval and request execution.
//!
//! Each modem runs one supervisor task. Everything that touches the serial
//! line goes through this task, so API requests can never interleave AT
//! commands.

use super::actor::{self, AtCommand, AtError, AtHandle};
use super::parser::{self, Domain, Urc};
use super::profiles::{ModemProfile, ProfileSet};
use super::state::*;
use super::transport::Connector;
use crate::config::{ModemConfig, SharedConfig, SimProfile};
use crate::database::Db;
use crate::events::Events;
use crate::metrics::Metrics;
use crate::utils;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, MissedTickBehavior};

/// Receives PDUs read from a modem. Returns true once the PDU is durably
/// stored (or recognised as a duplicate), which permits deleting it from
/// modem storage.
#[async_trait]
pub trait PduSink: Send + Sync {
    async fn on_pdu(&self, modem_id: &str, sim_id: Option<&str>, pdu: &str) -> bool;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallSignalKind {
    Ring,
    CallerId(String),
    Ended(String),
    /// An outgoing call was dialled through the gateway.
    Dialed(String),
    /// The modem connection was lost while a call may have been active.
    Lost,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSignal {
    pub modem_id: String,
    pub kind: CallSignalKind,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    /// The modem cannot send right now; nothing was transmitted.
    #[error("modem not ready: {0}")]
    NotReady(String),
    /// The modem or network rejected the message; nothing was delivered.
    #[error("rejected: {0}")]
    Rejected(String),
    /// The PDU was handed to the modem but no result arrived. The message may
    /// or may not have been transmitted.
    #[error("submission outcome unknown: {0}")]
    Ambiguous(String),
}

enum Req {
    SendPdu {
        tpdu_len: usize,
        pdu: String,
        reply: oneshot::Sender<Result<u8, SendError>>,
    },
    Ussd {
        code: String,
        reply: oneshot::Sender<Result<String, String>>,
    },
    RawAt {
        command: String,
        reply: oneshot::Sender<Result<Vec<String>, String>>,
    },
    Answer {
        reply: oneshot::Sender<Result<(), String>>,
    },
    Hangup {
        reply: oneshot::Sender<Result<(), String>>,
    },
    Reconnect,
}

/// Cloneable handle to a modem supervisor.
#[derive(Clone)]
pub struct Modem {
    pub id: String,
    status: watch::Receiver<ModemStatus>,
    tx: mpsc::Sender<Req>,
}

impl Modem {
    pub fn status(&self) -> ModemStatus {
        self.status.borrow().clone()
    }

    pub fn watch(&self) -> watch::Receiver<ModemStatus> {
        self.status.clone()
    }

    pub fn is_ready(&self) -> bool {
        self.status.borrow().is_ready()
    }

    /// Submit one SMS-SUBMIT PDU. Returns the modem's message reference.
    pub async fn send_pdu(&self, tpdu_len: usize, pdu: String) -> Result<u8, SendError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Req::SendPdu {
                tpdu_len,
                pdu,
                reply,
            })
            .await
            .map_err(|_| SendError::NotReady("modem task stopped".into()))?;
        // If the supervisor disappears mid-request the outcome is unknown.
        rx.await.unwrap_or_else(|_| {
            Err(SendError::Ambiguous(
                "modem task stopped during submission".into(),
            ))
        })
    }

    pub async fn ussd(&self, code: &str) -> Result<String, String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Req::Ussd {
                code: code.to_string(),
                reply,
            })
            .await
            .map_err(|_| "modem task stopped".to_string())?;
        rx.await
            .unwrap_or_else(|_| Err("modem task stopped".into()))
    }

    pub async fn raw_at(&self, command: &str) -> Result<Vec<String>, String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Req::RawAt {
                command: command.to_string(),
                reply,
            })
            .await
            .map_err(|_| "modem task stopped".to_string())?;
        rx.await
            .unwrap_or_else(|_| Err("modem task stopped".into()))
    }

    pub async fn answer(&self) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Req::Answer { reply })
            .await
            .map_err(|_| "modem task stopped".to_string())?;
        rx.await
            .unwrap_or_else(|_| Err("modem task stopped".into()))
    }

    pub async fn hangup(&self) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Req::Hangup { reply })
            .await
            .map_err(|_| "modem task stopped".to_string())?;
        rx.await
            .unwrap_or_else(|_| Err("modem task stopped".into()))
    }

    /// Drop the current connection (if any) and reconnect immediately.
    pub async fn reconnect(&self) -> bool {
        self.tx.send(Req::Reconnect).await.is_ok()
    }
}

#[derive(Clone)]
pub struct ModemDeps {
    pub cfg: SharedConfig,
    pub db: Db,
    pub events: Events,
    pub metrics: Arc<Metrics>,
    pub sink: Arc<dyn PduSink>,
    pub calls: mpsc::Sender<CallSignal>,
    pub connector: Arc<dyn Connector>,
    pub profiles: Arc<ProfileSet>,
}

enum End {
    Shutdown,
    /// Reconnect immediately (operator request or configuration change).
    Reconnect,
    Failed {
        reason: String,
        hint: Option<String>,
        quick_retry: bool,
    },
}

struct Worker {
    id: String,
    deps: ModemDeps,
    status: watch::Sender<ModemStatus>,
    rx: mpsc::Receiver<Req>,
    shutdown: watch::Receiver<bool>,
    attempts: u32,
    was_connected: bool,
    last_reported_error: Option<String>,
}

/// Start the supervisor for one modem.
pub fn spawn(
    id: &str,
    deps: ModemDeps,
    shutdown: watch::Receiver<bool>,
) -> (Modem, tokio::task::JoinHandle<()>) {
    let cfg = deps.cfg.load();
    let mc = cfg.modem(id).cloned().unwrap_or_default();
    let (status_tx, status_rx) = watch::channel(ModemStatus::new(id, &mc.device, mc.baud_rate));
    let (tx, rx) = mpsc::channel(64);
    let worker = Worker {
        id: id.to_string(),
        deps,
        status: status_tx,
        rx,
        shutdown,
        attempts: 0,
        was_connected: false,
        last_reported_error: None,
    };
    let handle = tokio::spawn(worker.run());
    (
        Modem {
            id: id.to_string(),
            status: status_rx,
            tx,
        },
        handle,
    )
}

fn open_hint(e: &std::io::Error, device: &str) -> String {
    let text = e.to_string().to_lowercase();
    if e.kind() == std::io::ErrorKind::NotFound
        || text.contains("no such file")
        || text.contains("no such device")
    {
        format!("{device} does not exist. Check that the module is attached and that modem.device names the right port (see /dev/serial/by-path/).")
    } else if e.kind() == std::io::ErrorKind::PermissionDenied || text.contains("permission denied")
    {
        format!("No permission to open {device}. The service user must be a member of the group owning the device (normally dialout).")
    } else if text.contains("busy") || text.contains("lock") {
        format!("{device} is in use by another process (ModemManager, a serial console/getty or a terminal program). Stop it or exclude the device.")
    } else {
        format!("Could not open {device}; check cabling, device name and permissions.")
    }
}

const NO_RESPONSE: &str = "The serial port opened but the modem does not answer AT commands. GSM modules such as the SIM868 are not powered on automatically: after attaching a module, hold its PWRKEY button for about two seconds until the NET LED blinks. Also check the baud rate and, on Waveshare HATs, that the UART jumpers select the interface in use (USB or GPIO). The gateway keeps retrying and connects as soon as the module answers.";

impl Worker {
    fn set_state(&self, state: ModemState) {
        self.status.send_if_modified(|s| {
            if s.state == state {
                return false;
            }
            tracing::info!(modem = %self.id, from = s.state.as_str(), to = state.as_str(), "modem state changed");
            s.state = state;
            s.state_since = utils::now();
            true
        });
        self.deps.metrics.set(
            "smsgw_modem_connected",
            &[("modem", &self.id)],
            if state == ModemState::Ready { 1.0 } else { 0.0 },
        );
    }

    fn update(&self, f: impl FnOnce(&mut ModemStatus)) {
        self.status.send_modify(f);
    }

    fn mc(&self) -> Option<ModemConfig> {
        self.deps.cfg.load().modem(&self.id).cloned()
    }

    async fn emit(&self, event: &str, mut data: serde_json::Value) {
        data["modem_id"] = json!(self.id);
        self.deps.events.emit(event, None, data).await;
    }

    async fn run(mut self) {
        loop {
            if *self.shutdown.borrow() {
                break;
            }
            let Some(mc) = self.mc().filter(|m| m.enabled) else {
                self.update(|s| s.enabled = false);
                self.set_state(ModemState::Disabled);
                if !self.idle(None).await {
                    break;
                }
                continue;
            };
            self.update(|s| {
                s.enabled = true;
                s.device = mc.device.clone();
                s.baud_rate = mc.baud_rate;
                s.reconnect_attempts = self.attempts;
            });
            self.set_state(if self.was_connected {
                ModemState::Recovering
            } else {
                ModemState::Connecting
            });
            if self.attempts > 0 {
                self.deps.metrics.inc(
                    "smsgw_modem_reconnect_attempts_total",
                    &[("modem", &self.id)],
                );
            }
            let end = match self.deps.connector.open(&mc).await {
                Err(e) => End::Failed {
                    reason: format!("cannot open {}: {e}", mc.device),
                    hint: Some(open_hint(&e, &mc.device)),
                    quick_retry: false,
                },
                Ok(stream) => {
                    let (at, urc) = actor::spawn(stream, self.id.clone());
                    self.session(&mc, at, urc).await
                }
            };
            match end {
                End::Shutdown => break,
                End::Reconnect => {
                    self.attempts = 0;
                    continue;
                }
                End::Failed {
                    reason,
                    hint,
                    quick_retry,
                } => {
                    let _ = self.deps.calls.try_send(CallSignal {
                        modem_id: self.id.clone(),
                        kind: CallSignalKind::Lost,
                    });
                    tracing::warn!(modem = %self.id, reason = %reason, "modem unavailable");
                    if self.was_connected {
                        self.emit("modem.disconnected", json!({"reason": reason}))
                            .await;
                        self.was_connected = false;
                    }
                    if self.last_reported_error.as_deref() != Some(reason.as_str()) {
                        self.emit("modem.error", json!({"error": reason, "hint": hint}))
                            .await;
                        self.last_reported_error = Some(reason.clone());
                    }
                    self.update(|s| {
                        s.last_error = Some(reason);
                        s.hint = hint;
                        s.network = NetworkStatus {
                            registration: "unknown".into(),
                            ..Default::default()
                        };
                        s.signal = SignalStatus::default();
                        s.connected_at = None;
                    });
                    self.set_state(ModemState::Disconnected);
                    self.attempts += 1;
                    // An unpowered module is expected to be switched on by hand
                    // shortly, so it is probed more often than a missing device.
                    let cap = if quick_retry {
                        mc.max_reconnect_delay_seconds.min(20)
                    } else {
                        mc.max_reconnect_delay_seconds
                    };
                    let delay =
                        utils::backoff_seconds(mc.reconnect_delay_seconds, cap, self.attempts);
                    let wait = mc.auto_reconnect.then(|| Duration::from_secs(delay));
                    if !self.idle(wait).await {
                        break;
                    }
                }
            }
        }
        self.set_state(ModemState::ShuttingDown);
    }

    /// Wait while disconnected. Requests are answered with "not ready".
    /// Returns false on shutdown. `None` waits for an explicit reconnect.
    async fn idle(&mut self, wait: Option<Duration>) -> bool {
        let deadline = wait.map(|w| Instant::now() + w);
        let mut reload = tokio::time::interval(Duration::from_secs(5));
        loop {
            let sleep = async {
                match deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                _ = sleep => return true,
                _ = self.shutdown.changed() => return false,
                _ = reload.tick(), if deadline.is_none() => {
                    // Disabled modems resume when the configuration enables them.
                    if self.mc().map(|m| m.enabled).unwrap_or(false) && self.status.borrow().state == ModemState::Disabled {
                        return true;
                    }
                }
                req = self.rx.recv() => match req {
                    None => return false,
                    Some(Req::Reconnect) => {
                        self.attempts = 0;
                        return true;
                    }
                    Some(other) => reject(other, "modem is not connected"),
                },
            }
        }
    }

    async fn q(&self, at: &AtHandle, cmd: &str, mc: &ModemConfig) -> Result<Vec<String>, AtError> {
        at.at(cmd, Duration::from_secs(mc.command_timeout_seconds))
            .await
    }

    /// Run a command whose failure is tolerable. `Err` only when the link is gone.
    async fn opt(
        &self,
        at: &AtHandle,
        cmd: &str,
        mc: &ModemConfig,
    ) -> Result<Option<Vec<String>>, End> {
        match self.q(at, cmd, mc).await {
            Ok(l) => Ok(Some(l)),
            Err(AtError::Closed) => Err(lost("serial connection closed")),
            Err(_) => Ok(None),
        }
    }

    async fn session(
        &mut self,
        mc: &ModemConfig,
        at: AtHandle,
        mut urc: mpsc::Receiver<Urc>,
    ) -> End {
        let init = match self.initialise(mc, &at).await {
            Ok(i) => i,
            Err(end) => return end,
        };
        self.attempts = 0;
        self.last_reported_error = None;
        self.was_connected = true;
        self.update(|s| {
            s.last_error = None;
            s.hint = None;
            s.reconnect_attempts = 0;
            s.connected_at = Some(utils::now());
            s.last_seen_at = Some(utils::now());
        });
        let st = self.status.borrow().clone();
        self.emit("modem.connected", json!({"manufacturer": st.manufacturer, "model": st.model, "firmware": st.firmware, "profile": st.profile})).await;
        self.main_loop(mc, &at, &mut urc, init).await
    }
}

fn lost(reason: &str) -> End {
    End::Failed {
        reason: reason.to_string(),
        hint: None,
        quick_retry: false,
    }
}

fn reject(req: Req, why: &str) {
    match req {
        Req::SendPdu { reply, .. } => {
            let _ = reply.send(Err(SendError::NotReady(why.to_string())));
        }
        Req::Ussd { reply, .. } => {
            let _ = reply.send(Err(why.to_string()));
        }
        Req::RawAt { reply, .. } => {
            let _ = reply.send(Err(why.to_string()));
        }
        Req::Answer { reply } | Req::Hangup { reply } => {
            let _ = reply.send(Err(why.to_string()));
        }
        Req::Reconnect => {}
    }
}

/// Result of initialisation carried into the main loop.
struct Init {
    profile: ModemProfile,
    sim: Option<SimProfile>,
    sim_ready: bool,
}

mod init;
mod session;
