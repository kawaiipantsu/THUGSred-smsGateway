//! Event bus: every event is persisted (together with its integration
//! deliveries, forming a durable outbox) and broadcast to live subscribers.

use crate::config::SharedConfig;
use crate::database::{Db, EventRecord, Message};
use crate::metrics::Metrics;
use crate::{routing, utils};
use serde_json::{json, Value as Json};
use std::sync::Arc;
use tokio::sync::{broadcast, Notify};

#[derive(Clone)]
pub struct Events {
    db: Db,
    cfg: SharedConfig,
    tx: broadcast::Sender<EventRecord>,
    wake: Arc<Notify>,
    metrics: Arc<Metrics>,
}

impl Events {
    pub fn new(db: Db, cfg: SharedConfig, metrics: Arc<Metrics>) -> Self {
        let (tx, _) = broadcast::channel(256);
        Self {
            db,
            cfg,
            tx,
            wake: Arc::new(Notify::new()),
            metrics,
        }
    }

    /// Live event stream (bounded; slow subscribers skip events).
    pub fn subscribe(&self) -> broadcast::Receiver<EventRecord> {
        self.tx.subscribe()
    }

    /// Notified whenever new integration deliveries are waiting.
    pub fn wake_handle(&self) -> Arc<Notify> {
        self.wake.clone()
    }

    /// Persist and publish an event. Failures are logged and never propagate:
    /// event delivery must not interfere with message handling.
    pub async fn emit(
        &self,
        event_type: &str,
        correlation_id: Option<&str>,
        data: Json,
    ) -> EventRecord {
        let cfg = self.cfg.load();
        let ev = EventRecord {
            id: utils::new_id(),
            event_type: event_type.to_string(),
            instance_id: cfg.general.instance_id.clone(),
            correlation_id: correlation_id.map(str::to_string),
            timestamp: utils::now(),
            data,
        };
        let dests = routing::destinations(&cfg, &ev);
        if let Err(e) = self.db.insert_event(&ev, &dests).await {
            tracing::error!(event = %ev.event_type, error = %e, "failed to persist event");
        } else if !dests.is_empty() {
            if let Some(id) = correlation_id
                .filter(|_| event_type.starts_with("sms.") || event_type.starts_with("mms."))
            {
                let _ = self.db.set_integration_status(id, "pending").await;
            }
            self.wake.notify_waiters();
        }
        self.metrics
            .inc("smsgw_events_total", &[("type", event_type)]);
        tracing::debug!(event = %ev.event_type, id = %ev.id, destinations = dests.len(), "event emitted");
        let _ = self.tx.send(ev.clone());
        ev
    }
}

/// Event payload describing a message.
pub fn message_data(m: &Message) -> Json {
    json!({
        "message": {
            "id": m.id,
            "direction": m.direction,
            "type": m.message_type,
            "sender": m.sender,
            "recipient": m.recipient,
            "body": m.body,
            "encoding": m.encoding,
            "class": m.message_class,
            "parts": m.parts,
            "src_port": m.src_port,
            "dst_port": m.dst_port,
            "modem_id": m.modem_id,
            "sim_id": m.sim_id,
            "status": m.status,
            "delivery_status": m.delivery_status,
            "error": m.error,
            "network_timestamp": m.network_timestamp,
            "received_at": m.received_at,
            "submitted_at": m.submitted_at,
            "delivered_at": m.delivered_at,
            "raw_pdu": m.raw_pdu,
            "payload": m.payload.as_ref().map(hex::encode_upper),
        }
    })
}
