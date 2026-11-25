//! Inbound PDU processing: deduplication, classification, multipart
//! assembly, delivery-report correlation and persistence.

use super::ota;
use super::pdu::decode::{Deliver, StatusReport};
use super::pdu::types::{decode_payload_text, Alphabet};
use super::pdu::{self, Pdu, PduDirection};
use super::service::SmsService;
use crate::config::Config;
use crate::database::{Message, Part};
use crate::events::message_data;
use crate::modem::PduSink;
use crate::{mms, utils};
use async_trait::async_trait;
use serde_json::json;

/// Identity of a concatenation group: modem, sender, reference, total, wide reference.
type GroupKey = (
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<i64>,
    bool,
);

fn alphabet(name: Option<&str>) -> Alphabet {
    match name {
        Some("ucs2") => Alphabet::Ucs2,
        Some("8bit") => Alphabet::Binary,
        _ => Alphabet::Gsm7,
    }
}

/// Classify a delivered message for storage and event routing.
fn classify(d: &Deliver, payload: &[u8]) -> (&'static str, Option<mms::MmsNotification>) {
    let ports = d.user_data.header.as_ref().and_then(|h| h.ports.as_ref());
    if ports
        .map(|p| p.destination == 2948 || p.destination == 2949)
        .unwrap_or(false)
    {
        return match mms::parse_wap_push(payload) {
            Some(n) => ("mms_notification", Some(n)),
            None => ("wap_push", None),
        };
    }
    // SIM Toolkit OTA (S@T/WIB browser pushes, Simjacker class) before the
    // generic service/binary checks, since it is class 2 / PID 0x7F binary.
    if ota::analyze(d, payload).is_some() {
        return (ota::OtaAnalysis::MESSAGE_TYPE, None);
    }
    if d.data_coding_scheme.message_waiting.is_some()
        || matches!(d.protocol_identifier, 0x40 | 0x7C..=0x7F)
    {
        return ("service", None);
    }
    if d.user_data.encoding == Alphabet::Binary || ports.is_some() {
        return ("binary", None);
    }
    if d.data_coding_scheme.message_class == Some(0) {
        return ("flash", None);
    }
    ("sms", None)
}

fn event_for(message_type: &str) -> &'static str {
    match message_type {
        "sim_toolkit" => "sms.ota_toolkit",
        "binary" => "sms.binary_received",
        "service" | "wap_push" => "sms.service_received",
        "mms_notification" => "mms.notification",
        _ => "sms.received",
    }
}

#[async_trait]
impl PduSink for SmsService {
    async fn on_pdu(&self, modem_id: &str, sim_id: Option<&str>, pdu: &str) -> bool {
        match self.ingest(modem_id, sim_id, pdu).await {
            Ok(()) => true,
            Err(e) => {
                // Not persisted: the PDU stays on the modem and is retried.
                tracing::error!(modem = %modem_id, error = %e, "failed to persist inbound PDU; leaving it in modem storage");
                self.events.emit("gateway.error", None, json!({"error": format!("inbound message could not be stored: {e}"), "modem_id": modem_id})).await;
                false
            }
        }
    }
}

impl SmsService {
    /// Process one PDU read from a modem. `Ok` means it is durably handled.
    pub async fn ingest(
        &self,
        modem_id: &str,
        sim_id: Option<&str>,
        pdu_hex: &str,
    ) -> Result<(), crate::database::DbError> {
        let cfg = self.cfg.load_full();
        let pdu_hex = pdu_hex.trim().to_ascii_uppercase();
        // The fingerprint covers the whole PDU (sender, service-centre
        // timestamp, headers and content), scoped to the receiving modem.
        let mut fingerprint = utils::sha256_hex(format!("{modem_id}:{pdu_hex}").as_bytes());
        if !cfg.sms.deduplication_enabled {
            // Without deduplication every PDU is stored, so the fingerprint
            // (which is unique in the database) is made unique per ingestion.
            fingerprint = format!("{fingerprint}:{}", utils::new_id());
        } else if self.db.fingerprint_known(&fingerprint).await? {
            tracing::debug!(modem = %modem_id, "duplicate PDU ignored");
            return Ok(());
        }
        let result = match pdu::decode(&pdu_hex, PduDirection::MobileTerminated, true) {
            Ok(Pdu::Deliver(d)) => {
                self.on_deliver(&cfg, modem_id, sim_id, &pdu_hex, &fingerprint, *d)
                    .await
            }
            Ok(Pdu::StatusReport(r)) => {
                self.on_status_report(&cfg, modem_id, &pdu_hex, &fingerprint, *r)
                    .await
            }
            Ok(Pdu::Submit(_)) => {
                self.store_raw(
                    &cfg,
                    modem_id,
                    sim_id,
                    &pdu_hex,
                    &fingerprint,
                    "unexpected SMS-SUBMIT in inbound path",
                )
                .await
            }
            Err(e) => {
                self.store_raw(
                    &cfg,
                    modem_id,
                    sim_id,
                    &pdu_hex,
                    &fingerprint,
                    &e.to_string(),
                )
                .await
            }
        };
        match result {
            Err(e) if e.is_unique_violation() => Ok(()), // concurrent duplicate
            other => other,
        }
    }

    /// Keep an undecodable PDU as a recoverable raw record.
    async fn store_raw(
        &self,
        cfg: &Config,
        modem_id: &str,
        sim_id: Option<&str>,
        pdu_hex: &str,
        fingerprint: &str,
        error: &str,
    ) -> Result<(), crate::database::DbError> {
        let now = utils::now();
        let m = Message {
            id: utils::new_id(),
            direction: "inbound".into(),
            message_type: "unknown".into(),
            instance_id: cfg.general.instance_id.clone(),
            modem_id: Some(modem_id.into()),
            sim_id: sim_id.map(str::to_string),
            raw_pdu: Some(pdu_hex.into()),
            status: "malformed".into(),
            error: Some(error.into()),
            fingerprint: Some(fingerprint.into()),
            received_at: Some(now.clone()),
            created_at: now.clone(),
            updated_at: now,
            parts: 1,
            ..Default::default()
        };
        self.db.insert_message(&m, &[]).await?;
        tracing::warn!(target: "messages", message_id = %m.id, modem = %modem_id, error, "undecodable PDU stored as raw record");
        Ok(())
    }

    async fn on_deliver(
        &self,
        cfg: &Config,
        modem_id: &str,
        sim_id: Option<&str>,
        pdu_hex: &str,
        fingerprint: &str,
        d: Deliver,
    ) -> Result<(), crate::database::DbError> {
        let now = utils::now();
        let header = d.user_data.header.clone().unwrap_or_default();
        let shifts = (header.national_single_shift.unwrap_or(0) as i64) * 256
            + header.national_locking_shift.unwrap_or(0) as i64;
        let concat = header
            .concatenation
            .clone()
            .filter(|c| c.total > 1 && c.sequence >= 1 && c.sequence <= c.total);
        let part = Part {
            id: utils::new_id(),
            direction: "inbound".into(),
            modem_id: Some(modem_id.into()),
            address: Some(d.sender.number.clone()),
            concat_ref: concat.as_ref().map(|c| c.reference as i64),
            concat_total: concat.as_ref().map(|c| c.total as i64),
            sequence: concat.as_ref().map(|c| c.sequence as i64).unwrap_or(1),
            wide_ref: concat.as_ref().map(|c| c.wide_reference).unwrap_or(false),
            encoding: Some(d.user_data.encoding.as_str().into()),
            shifts,
            pdu: Some(pdu_hex.into()),
            payload: Some(d.user_data.payload.clone()),
            status: "pending".into(),
            created_at: now.clone(),
            ..Default::default()
        };
        let Some(c) = concat else {
            // Single-part message: message and part are stored atomically.
            let mut part = part;
            part.status = "assembled".into();
            let mut m = self.build_message(
                cfg,
                modem_id,
                sim_id,
                &d,
                pdu_hex,
                &d.user_data.payload,
                shifts,
                1,
                &[],
            );
            m.fingerprint = Some(fingerprint.into());
            part.message_id = Some(m.id.clone());
            self.db.insert_message(&m, &[part]).await?;
            self.after_store(cfg, m, false).await;
            return Ok(());
        };

        // Concatenated message: persist the segment, then try to assemble.
        let (reference, total) = (c.reference as i64, c.total as i64);
        let existing = self
            .db
            .pending_group(
                modem_id,
                &d.sender.number,
                reference,
                total,
                c.wide_reference,
            )
            .await?;
        if existing.iter().any(|p| p.sequence == c.sequence as i64) {
            // Same slot, different content: the reference number was reused
            // by a new message. Flush what was collected for the old one.
            self.flush_group(cfg, existing).await?;
        }
        let mut part = part;
        part.fingerprint = Some(fingerprint.into());
        self.db.insert_part(&part).await?;
        let group = self
            .db
            .pending_group(
                modem_id,
                &d.sender.number,
                reference,
                total,
                c.wide_reference,
            )
            .await?;
        let complete = (1..=total).all(|s| group.iter().any(|p| p.sequence == s));
        if complete {
            self.assemble(cfg, group, sim_id, true).await?;
        }
        Ok(())
    }

    /// Build the message record from the (first) deliver PDU and the full payload.
    #[allow(clippy::too_many_arguments)]
    fn build_message(
        &self,
        cfg: &Config,
        modem_id: &str,
        sim_id: Option<&str>,
        d: &Deliver,
        pdu_hex: &str,
        payload: &[u8],
        shifts: i64,
        parts: i64,
        missing: &[i64],
    ) -> Message {
        let now = utils::now();
        let (message_type, notification) = classify(d, payload);
        let enc = d.user_data.encoding;
        let text = if d.data_coding_scheme.compressed {
            None
        } else {
            decode_payload_text(enc, payload, (shifts / 256) as u8, (shifts % 256) as u8)
        };
        let header = d.user_data.header.as_ref();
        let concat = header.and_then(|h| h.concatenation.as_ref());
        let ports = header.and_then(|h| h.ports.as_ref());
        let mut metadata = json!({});
        if let Some(mwi) = &d.data_coding_scheme.message_waiting {
            metadata["message_waiting"] = json!(mwi);
        }
        if d.protocol_identifier != 0 {
            metadata["protocol"] = json!(d.protocol_description);
        }
        if !d.user_data.warnings.is_empty() {
            metadata["warnings"] = json!(d.user_data.warnings);
        }
        if !missing.is_empty() {
            metadata["missing_parts"] = json!(missing);
        }
        if let Some(ota) = ota::analyze(d, payload) {
            metadata["sim_toolkit"] = json!(ota);
        }
        let (mut sender, mut body) = (d.sender.number.clone(), text);
        if let Some(n) = &notification {
            metadata["mms"] = json!(n);
            metadata["sms_sender"] = json!(d.sender.number);
            if let Some(from) = n.from.clone().filter(|f| !f.is_empty()) {
                sender = from;
            }
            body = Some(
                n.subject
                    .clone()
                    .unwrap_or_else(|| "MMS notification".into()),
            );
        }
        let binary = enc == Alphabet::Binary || d.data_coding_scheme.compressed;
        let cfg_sim = sim_id.and_then(|s| cfg.sim(s));
        Message {
            id: utils::new_id(),
            direction: "inbound".into(),
            message_type: message_type.into(),
            instance_id: cfg.general.instance_id.clone(),
            modem_id: Some(modem_id.into()),
            sim_id: sim_id.map(str::to_string),
            normalized_number: Some(utils::normalize_number(&sender)),
            original_address: Some(d.sender.number.clone()),
            sender: Some(sender),
            recipient: cfg_sim
                .map(|s| s.mobile_number.clone())
                .filter(|n| !n.is_empty()),
            body,
            raw_pdu: cfg.sms.store_raw_pdu.then(|| pdu_hex.to_string()),
            payload: binary.then(|| payload.to_vec()),
            encoding: Some(enc.as_str().into()),
            message_class: d.data_coding_scheme.message_class.map(i64::from),
            protocol_id: Some(d.protocol_identifier as i64),
            data_coding: Some(d.data_coding_scheme.raw as i64),
            smsc: d.smsc.as_ref().map(|s| s.number.clone()),
            concat_ref: concat.map(|c| c.reference as i64),
            concat_total: concat.map(|c| c.total as i64),
            parts,
            src_port: ports.map(|p| p.source as i64),
            dst_port: ports.map(|p| p.destination as i64),
            network_timestamp: d.timestamp.utc.map(utils::ts),
            received_at: Some(now.clone()),
            status: if missing.is_empty() {
                "received"
            } else {
                "incomplete"
            }
            .into(),
            metadata: metadata
                .as_object()
                .filter(|o| !o.is_empty())
                .map(|_| metadata.clone()),
            created_at: now.clone(),
            updated_at: now,
            ..Default::default()
        }
    }

    /// Assemble a group of segments (complete or not) into one message.
    async fn assemble(
        &self,
        cfg: &Config,
        mut group: Vec<Part>,
        sim_id: Option<&str>,
        complete: bool,
    ) -> Result<(), crate::database::DbError> {
        group.sort_by_key(|p| p.sequence);
        group.dedup_by_key(|p| p.sequence);
        let Some(first) = group.first().cloned() else {
            return Ok(());
        };
        let modem_id = first.modem_id.clone().unwrap_or_default();
        let total = first.concat_total.unwrap_or(group.len() as i64);
        let Some(Ok(Pdu::Deliver(d))) = first
            .pdu
            .as_deref()
            .map(|p| pdu::decode(p, PduDirection::MobileTerminated, true))
        else {
            return Ok(());
        };
        let enc = alphabet(first.encoding.as_deref());
        let mut payload = Vec::new();
        let mut missing = Vec::new();
        for seq in 1..=total {
            match group.iter().find(|p| p.sequence == seq) {
                Some(p) => payload.extend_from_slice(p.payload.as_deref().unwrap_or(&[])),
                None => {
                    missing.push(seq);
                    // Mark the gap in text messages so the loss is visible.
                    match enc {
                        Alphabet::Gsm7 => {
                            payload.extend(pdu::gsm7::encode_str("[...]").unwrap_or_default())
                        }
                        Alphabet::Ucs2 => {
                            payload.extend("[...]".encode_utf16().flat_map(|u| u.to_be_bytes()))
                        }
                        Alphabet::Binary => {}
                    }
                }
            }
        }
        let sim_id = sim_id
            .map(str::to_string)
            .or_else(|| cfg.sim_for_modem(&modem_id).map(|s| s.id.clone()));
        let m = self.build_message(
            cfg,
            &modem_id,
            sim_id.as_deref(),
            &d,
            first.pdu.as_deref().unwrap_or(""),
            &payload,
            first.shifts,
            total,
            &missing,
        );
        let ids: Vec<String> = group.iter().map(|p| p.id.clone()).collect();
        self.db.insert_assembled(&m, &ids).await?;
        self.after_store(cfg, m, complete).await;
        Ok(())
    }

    async fn flush_group(
        &self,
        cfg: &Config,
        group: Vec<Part>,
    ) -> Result<(), crate::database::DbError> {
        if group.is_empty() {
            return Ok(());
        }
        tracing::warn!(parts = group.len(), "storing incomplete multipart message");
        self.assemble(cfg, group, None, false).await
    }

    /// Store multipart messages whose remaining segments never arrived.
    pub async fn flush_stale_multipart(&self) -> Result<usize, crate::database::DbError> {
        let cfg = self.cfg.load_full();
        let cutoff = utils::ts_after(-(cfg.sms.multipart_timeout_seconds as i64));
        let stale = self.db.stale_pending_parts(&cutoff).await?;
        let mut flushed = 0;
        let mut seen: Vec<GroupKey> = Vec::new();
        for p in stale {
            let key = (
                p.modem_id.clone(),
                p.address.clone(),
                p.concat_ref,
                p.concat_total,
                p.wide_ref,
            );
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            let group = self
                .db
                .pending_group(
                    p.modem_id.as_deref().unwrap_or(""),
                    p.address.as_deref().unwrap_or(""),
                    p.concat_ref.unwrap_or(0),
                    p.concat_total.unwrap_or(0),
                    p.wide_ref,
                )
                .await?;
            // Only flush when the newest segment is stale too.
            if group.iter().all(|g| g.created_at < cutoff) {
                self.flush_group(&cfg, group).await?;
                flushed += 1;
            }
        }
        Ok(flushed)
    }

    /// Logging, metrics, events and automation after a message is stored.
    async fn after_store(&self, cfg: &Config, m: Message, multipart_completed: bool) {
        self.log_message("received", &m, cfg);
        self.metrics.inc(
            "smsgw_messages_received_total",
            &[
                ("modem", m.modem_id.as_deref().unwrap_or("")),
                ("type", &m.message_type),
            ],
        );
        let service_like = matches!(m.message_type.as_str(), "service" | "wap_push");
        if service_like && !cfg.sms.allow_service_messages {
            // Stored for the record, but not announced or forwarded.
            return;
        }
        let data = message_data(&m);
        let ev = self
            .events
            .emit(event_for(&m.message_type), Some(&m.id), data.clone())
            .await;
        if multipart_completed {
            self.events
                .emit("sms.multipart_completed", Some(&m.id), data)
                .await;
        }
        // Honeypot alert: a SIM Toolkit OTA push (possible Simjacker/WIBattack)
        // also raises a dedicated security event for real-time collectors.
        if m.message_type == ota::OtaAnalysis::MESSAGE_TYPE {
            self.events
                .emit("security.sim_attack", Some(&m.id), message_data(&m))
                .await;
        }
        if m.message_type == "sms" && m.status == "received" {
            self.run_automation(cfg, &m, &ev).await;
        }
        if m.message_type == "mms_notification" && cfg.mms.enabled && cfg.mms.download_attachments {
            let me = self.clone();
            tokio::spawn(async move { me.download_mms(m).await });
        }
    }

    /// Fetch the MMS announced by a notification from the configured MMSC.
    async fn download_mms(&self, m: Message) {
        let cfg = self.cfg.load_full();
        let Some(location) = m
            .metadata
            .as_ref()
            .and_then(|j| j["mms"]["content_location"].as_str())
            .map(str::to_string)
        else {
            return;
        };
        let txid = m
            .metadata
            .as_ref()
            .and_then(|j| j["mms"]["transaction_id"].as_str())
            .unwrap_or("mms")
            .to_string();
        match mms::fetch(&cfg.mms, &location).await {
            Ok(bytes) => {
                let dir = std::path::Path::new(&cfg.paths.attachments);
                let name = format!("{}-{}.mms", m.id, utils::sanitize_filename(&txid));
                let path = dir.join(&name);
                let write = async {
                    tokio::fs::create_dir_all(dir).await?;
                    tokio::fs::write(&path, &bytes).await
                };
                match write.await {
                    Ok(()) => {
                        let sha = utils::sha256_hex(&bytes);
                        let _ = self
                            .db
                            .insert_attachment(
                                &m.id,
                                &name,
                                "application/vnd.wap.mms-message",
                                bytes.len() as i64,
                                &path.display().to_string(),
                                &sha,
                            )
                            .await;
                        let _ = self
                            .db
                            .add_history(
                                &m.id,
                                "mms_downloaded",
                                Some(&format!("{} bytes", bytes.len())),
                            )
                            .await;
                        self.events
                            .emit("mms.received", Some(&m.id), message_data(&m))
                            .await;
                    }
                    Err(e) => {
                        tracing::error!(message_id = %m.id, error = %e, "could not store MMS attachment")
                    }
                }
            }
            Err(e) => {
                tracing::warn!(message_id = %m.id, error = %e, "MMS retrieval failed");
                let _ = self
                    .db
                    .add_history(&m.id, "mms_download_failed", Some(&e.to_string()))
                    .await;
            }
        }
    }

    async fn on_status_report(
        &self,
        cfg: &Config,
        modem_id: &str,
        pdu_hex: &str,
        fingerprint: &str,
        r: StatusReport,
    ) -> Result<(), crate::database::DbError> {
        let since = utils::ts_after(-(cfg.sms.delivery_report_window_days as i64) * 86400);
        let candidates = self
            .db
            .parts_by_modem_ref(modem_id, r.message_reference as i64, &since)
            .await?;
        let part = candidates.into_iter().find(|p| {
            p.address
                .as_deref()
                .map(|a| utils::numbers_match(a, &r.recipient.number))
                .unwrap_or(false)
        });
        let message_id = part.as_ref().and_then(|p| p.message_id.clone());
        self.db
            .insert_delivery_report(
                message_id.as_deref(),
                part.as_ref().map(|p| p.id.as_str()),
                modem_id,
                r.message_reference as i64,
                &r.recipient.number,
                r.status as i64,
                r.status_category,
                r.status_description,
                r.timestamp.utc.map(utils::ts),
                r.discharge_time.utc.map(utils::ts),
                cfg.sms.store_raw_pdu.then_some(pdu_hex),
                fingerprint,
            )
            .await?;
        let (Some(part), Some(message_id)) = (part, message_id) else {
            tracing::info!(target: "messages", modem = %modem_id, reference = r.message_reference, category = r.status_category, "delivery report could not be correlated with a sent message");
            return Ok(());
        };
        let part_status = match r.status_category {
            "delivered" => "delivered",
            "pending" => {
                self.db
                    .add_history(
                        &message_id,
                        "submitted",
                        Some(&format!("delivery pending: {}", r.status_description)),
                    )
                    .await?;
                return Ok(());
            }
            _ => "delivery_failed",
        };
        self.db
            .update_part(
                &part.id,
                part_status,
                None,
                (part_status != "delivered").then_some(r.status_description),
            )
            .await?;
        let (total, delivered, failed) = self.db.part_delivery_counts(&message_id).await?;
        let new_state = if failed > 0 {
            Some("delivery_failed")
        } else if delivered == total {
            Some("delivered")
        } else {
            None
        };
        let Some(state) = new_state else {
            return Ok(());
        };
        let current = self.db.get_message(&message_id).await?;
        if current.as_ref().map(|m| m.status == state).unwrap_or(true) {
            return Ok(());
        }
        let error = (state == "delivery_failed")
            .then(|| format!("delivery failed: {}", r.status_description));
        self.db
            .set_message_state(&message_id, state, error.as_deref(), false, None)
            .await?;
        if let Some(m) = self.db.get_message(&message_id).await? {
            self.log_message(state, &m, cfg);
            let modem = m.modem_id.clone().unwrap_or_default();
            if state == "delivered" {
                self.metrics
                    .inc("smsgw_messages_delivered_total", &[("modem", &modem)]);
                self.events
                    .emit("sms.delivered", Some(&m.id), message_data(&m))
                    .await;
            } else {
                self.metrics.inc(
                    "smsgw_message_failures_total",
                    &[("modem", &modem), ("reason", "delivery")],
                );
                self.events
                    .emit("sms.failed", Some(&m.id), message_data(&m))
                    .await;
            }
        }
        Ok(())
    }
}
