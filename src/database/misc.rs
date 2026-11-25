//! Events, integration deliveries, calls, diagnostics, statistics and retention.

use super::*;
use crate::params;

fn event_from_row(r: &AnyRow) -> EventRecord {
    EventRecord {
        id: get_s(r, "id").unwrap_or_default(),
        event_type: get_s(r, "event_type").unwrap_or_default(),
        instance_id: get_s(r, "instance_id").unwrap_or_default(),
        correlation_id: get_s(r, "correlation_id"),
        timestamp: get_s(r, "created_at").unwrap_or_default(),
        data: get_s(r, "data")
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Json::Null),
    }
}

fn page(ps: &mut Vec<P>, limit: i64, offset: i64) {
    ps.push(limit.clamp(1, 1000).into());
    ps.push(offset.max(0).into());
}

impl Db {
    // ---- events and integration deliveries -------------------------------

    /// Persist an event together with its integration deliveries (outbox).
    pub async fn insert_event(&self, e: &EventRecord, integrations: &[String]) -> DbResult<()> {
        let mut tx = self.begin().await?;
        Self::tx_exec(
            &mut tx,
            "INSERT INTO events (id, event_type, instance_id, correlation_id, data, created_at) VALUES (?, ?, ?, ?, ?, ?)",
            params![&e.id, &e.event_type, &e.instance_id, &e.correlation_id, e.data.to_string(), &e.timestamp],
        )
        .await?;
        for name in integrations {
            Self::tx_exec(
                &mut tx,
                "INSERT INTO integration_events (id, event_id, event_type, integration, status, attempts, created_at, updated_at) VALUES (?, ?, ?, ?, 'pending', 0, ?, ?)",
                params![utils::new_id(), &e.id, &e.event_type, name, &e.timestamp, &e.timestamp],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn get_event(&self, id: &str) -> DbResult<Option<EventRecord>> {
        Ok(self
            .row("SELECT id, event_type, instance_id, correlation_id, data, created_at FROM events WHERE id = ?", params![id])
            .await?
            .as_ref()
            .map(event_from_row))
    }

    pub async fn list_events(
        &self,
        type_prefix: Option<&str>,
        since: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> DbResult<(Vec<EventRecord>, i64)> {
        let mut wh = vec!["1 = 1".to_string()];
        let mut ps: Vec<P> = vec![];
        if let Some(t) = type_prefix.filter(|t| !t.is_empty()) {
            wh.push("event_type LIKE ? ESCAPE '!'".into());
            let p = super::messages::like_pattern(t);
            ps.push(p[1..].to_string().into());
        }
        if let Some(s) = since.filter(|t| !t.is_empty()) {
            wh.push("created_at >= ?".into());
            ps.push(s.into());
        }
        let wh = wh.join(" AND ");
        let total = self
            .count(
                &format!("SELECT COUNT(*) FROM events WHERE {wh}"),
                ps.clone(),
            )
            .await?;
        page(&mut ps, limit, offset);
        let rows = self
            .rows(
                &format!("SELECT id, event_type, instance_id, correlation_id, data, created_at FROM events WHERE {wh} ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?"),
                ps,
            )
            .await?;
        Ok((rows.iter().map(event_from_row).collect(), total))
    }

    /// Claim deliveries that are due. Claimed rows move to `processing`.
    pub async fn claim_deliveries(&self, limit: i64) -> DbResult<Vec<Delivery>> {
        let now = utils::now();
        let rows = self
            .rows(
                "SELECT id, event_id, event_type, integration, status, attempts FROM integration_events WHERE status IN ('pending', 'retrying') AND (next_attempt_at IS NULL OR next_attempt_at <= ?) ORDER BY created_at ASC LIMIT ?",
                params![&now, limit],
            )
            .await?;
        let mut out = Vec::new();
        for r in rows {
            let d = Delivery {
                id: get_s(&r, "id").unwrap_or_default(),
                event_id: get_s(&r, "event_id").unwrap_or_default(),
                event_type: get_s(&r, "event_type").unwrap_or_default(),
                integration: get_s(&r, "integration").unwrap_or_default(),
                status: "processing".into(),
                attempts: get_i(&r, "attempts").unwrap_or(0),
            };
            let n = self
                .exec(
                    "UPDATE integration_events SET status = 'processing', updated_at = ? WHERE id = ? AND status IN ('pending', 'retrying')",
                    params![&now, &d.id],
                )
                .await?;
            if n == 1 {
                out.push(d);
            }
        }
        Ok(out)
    }

    /// Record the outcome of a delivery attempt and move the delivery on.
    /// `status` is delivered, retrying, dead or (for deferrals) pending.
    #[allow(clippy::too_many_arguments)]
    pub async fn finish_delivery(
        &self,
        d: &Delivery,
        status: &str,
        counted: bool,
        status_code: Option<i64>,
        error: Option<&str>,
        duration_ms: i64,
        next_attempt_at: Option<String>,
    ) -> DbResult<()> {
        let now = utils::now();
        let attempts = d.attempts + counted as i64;
        let delivered_at = (status == "delivered").then(|| now.clone());
        self.exec(
            "UPDATE integration_events SET status = ?, attempts = ?, next_attempt_at = ?, last_error = ?, updated_at = ?, delivered_at = ? WHERE id = ?",
            params![status, attempts, next_attempt_at, error, &now, delivered_at, &d.id],
        )
        .await?;
        if counted {
            self.exec(
                "INSERT INTO integration_attempts (delivery_id, integration, attempt, success, status_code, error, duration_ms, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                params![&d.id, &d.integration, attempts, status == "delivered", status_code, error, duration_ms, &now],
            )
            .await?;
        }
        if status == "dead" {
            self.exec(
                "INSERT INTO dead_letter_queue (id, kind, reference_id, integration, event_id, reason, payload, created_at) VALUES (?, 'integration', ?, ?, ?, ?, NULL, ?)",
                params![utils::new_id(), &d.id, &d.integration, &d.event_id, error, &now],
            )
            .await?;
        }
        Ok(())
    }

    /// Deliveries left in `processing` by a previous run are retried
    /// (at-least-once delivery); with `discard` they are dropped instead.
    pub async fn recover_deliveries(&self, discard: bool) -> DbResult<u64> {
        let now = utils::now();
        if discard {
            self.exec(
                "UPDATE integration_events SET status = 'dead', last_error = 'discarded at startup (persistent_queue disabled)', updated_at = ? WHERE status IN ('pending', 'retrying', 'processing')",
                params![&now],
            )
            .await
        } else {
            self.exec(
                "UPDATE integration_events SET status = 'retrying', next_attempt_at = NULL, updated_at = ? WHERE status = 'processing'",
                params![&now],
            )
            .await
        }
    }

    /// Queue a dead or failed delivery again.
    pub async fn replay_delivery(&self, id: &str) -> DbResult<bool> {
        let n = self
            .exec(
                "UPDATE integration_events SET status = 'pending', attempts = 0, next_attempt_at = NULL, last_error = NULL, updated_at = ? WHERE id = ? AND status IN ('dead', 'retrying')",
                params![utils::now(), id],
            )
            .await?;
        if n == 1 {
            self.exec(
                "DELETE FROM dead_letter_queue WHERE reference_id = ?",
                params![id],
            )
            .await?;
        }
        Ok(n == 1)
    }

    pub async fn list_deliveries(
        &self,
        integration: Option<&str>,
        status: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> DbResult<(Vec<Json>, i64)> {
        let mut wh = vec!["1 = 1".to_string()];
        let mut ps: Vec<P> = vec![];
        if let Some(i) = integration.filter(|s| !s.is_empty()) {
            wh.push("integration = ?".into());
            ps.push(i.into());
        }
        if let Some(s) = status.filter(|s| !s.is_empty()) {
            wh.push("status = ?".into());
            ps.push(s.into());
        }
        let wh = wh.join(" AND ");
        let total = self
            .count(
                &format!("SELECT COUNT(*) FROM integration_events WHERE {wh}"),
                ps.clone(),
            )
            .await?;
        page(&mut ps, limit, offset);
        let rows = self
            .json_rows(
                &format!("SELECT id, event_id, event_type, integration, status, attempts, next_attempt_at, last_error, created_at, updated_at, delivered_at FROM integration_events WHERE {wh} ORDER BY created_at DESC LIMIT ? OFFSET ?"),
                ps,
            )
            .await?;
        Ok((rows, total))
    }

    pub async fn list_dead_letters(&self, limit: i64, offset: i64) -> DbResult<(Vec<Json>, i64)> {
        let total = self
            .count("SELECT COUNT(*) FROM dead_letter_queue", vec![])
            .await?;
        let mut ps = vec![];
        page(&mut ps, limit, offset);
        let rows = self
            .json_rows(
                "SELECT id, kind, reference_id, integration, event_id, reason, created_at FROM dead_letter_queue ORDER BY created_at DESC LIMIT ? OFFSET ?",
                ps,
            )
            .await?;
        Ok((rows, total))
    }

    pub async fn add_dead_letter(
        &self,
        kind: &str,
        reference_id: &str,
        reason: &str,
        payload: Option<String>,
    ) -> DbResult<()> {
        self.exec(
            "INSERT INTO dead_letter_queue (id, kind, reference_id, integration, event_id, reason, payload, created_at) VALUES (?, ?, ?, NULL, NULL, ?, ?, ?)",
            params![utils::new_id(), kind, reference_id, reason, payload, utils::now()],
        )
        .await?;
        Ok(())
    }

    /// Delivery statistics for one integration.
    pub async fn integration_stats(&self, name: &str) -> DbResult<Json> {
        let c = |status: &'static str| async move {
            self.count(
                "SELECT COUNT(*) FROM integration_events WHERE integration = ? AND status = ?",
                params![name, status],
            )
            .await
        };
        let last = self
            .row(
                "SELECT success, status_code, error, duration_ms, created_at FROM integration_attempts WHERE integration = ? ORDER BY id DESC LIMIT 1",
                params![name],
            )
            .await?
            .as_ref()
            .map(row_json);
        let last_success = self
            .row(
                "SELECT MAX(delivered_at) AS t FROM integration_events WHERE integration = ? AND status = 'delivered'",
                params![name],
            )
            .await?
            .and_then(|r| get_s(&r, "t"));
        Ok(json!({
            "delivered": c("delivered").await?,
            "pending": c("pending").await? + c("processing").await?,
            "retrying": c("retrying").await?,
            "dead": c("dead").await?,
            "last_attempt": last,
            "last_success_at": last_success,
        }))
    }

    // ---- calls, recordings, attachments ----------------------------------

    pub async fn insert_call(
        &self,
        id: &str,
        modem_id: &str,
        direction: &str,
        number: Option<&str>,
        state: &str,
    ) -> DbResult<()> {
        self.exec(
            "INSERT INTO calls (id, modem_id, direction, number, state, answered, missed, started_at) VALUES (?, ?, ?, ?, ?, 0, 0, ?)",
            params![id, modem_id, direction, number, state, utils::now()],
        )
        .await?;
        Ok(())
    }

    pub async fn set_call_number(&self, id: &str, number: &str) -> DbResult<()> {
        self.exec(
            "UPDATE calls SET number = ? WHERE id = ? AND number IS NULL",
            params![number, id],
        )
        .await?;
        Ok(())
    }

    pub async fn set_call_answered(&self, id: &str) -> DbResult<()> {
        self.exec(
            "UPDATE calls SET state = 'answered', answered = 1, answered_at = ? WHERE id = ?",
            params![utils::now(), id],
        )
        .await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn finish_call(
        &self,
        id: &str,
        state: &str,
        missed: bool,
        duration: Option<i64>,
        reason: Option<&str>,
        recording_status: &str,
        recording_id: Option<&str>,
    ) -> DbResult<()> {
        self.exec(
            "UPDATE calls SET state = ?, missed = ?, ended_at = ?, duration_seconds = ?, termination_reason = ?, recording_status = ?, recording_id = ? WHERE id = ?",
            params![state, missed, utils::now(), duration, reason, recording_status, recording_id, id],
        )
        .await?;
        Ok(())
    }

    /// Calls left open by a previous run are closed as interrupted.
    pub async fn close_open_calls(&self) -> DbResult<u64> {
        self.exec(
            "UPDATE calls SET state = 'ended', ended_at = ?, termination_reason = 'gateway_restart' WHERE ended_at IS NULL",
            params![utils::now()],
        )
        .await
    }

    pub async fn list_calls(&self, limit: i64, offset: i64) -> DbResult<(Vec<Json>, i64)> {
        let total = self.count("SELECT COUNT(*) FROM calls", vec![]).await?;
        let mut ps = vec![];
        page(&mut ps, limit, offset);
        Ok((
            self.json_rows(
                "SELECT * FROM calls ORDER BY started_at DESC LIMIT ? OFFSET ?",
                ps,
            )
            .await?,
            total,
        ))
    }

    pub async fn get_call(&self, id: &str) -> DbResult<Option<Json>> {
        Ok(self
            .row("SELECT * FROM calls WHERE id = ?", params![id])
            .await?
            .as_ref()
            .map(row_json))
    }

    pub async fn insert_recording(
        &self,
        id: &str,
        call_id: &str,
        path: &str,
        format: &str,
        duration: i64,
        size: i64,
    ) -> DbResult<()> {
        self.exec(
            "INSERT INTO recordings (id, call_id, path, format, duration_seconds, size_bytes, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
            params![id, call_id, path, format, duration, size, utils::now()],
        )
        .await?;
        Ok(())
    }

    pub async fn list_recordings(&self, limit: i64, offset: i64) -> DbResult<(Vec<Json>, i64)> {
        let total = self
            .count("SELECT COUNT(*) FROM recordings", vec![])
            .await?;
        let mut ps = vec![];
        page(&mut ps, limit, offset);
        Ok((
            self.json_rows(
                "SELECT id, call_id, format, duration_seconds, size_bytes, created_at FROM recordings ORDER BY created_at DESC LIMIT ? OFFSET ?",
                ps,
            )
            .await?,
            total,
        ))
    }

    pub async fn recording_path(&self, id: &str) -> DbResult<Option<String>> {
        Ok(self
            .row("SELECT path FROM recordings WHERE id = ?", params![id])
            .await?
            .and_then(|r| get_s(&r, "path")))
    }

    pub async fn insert_attachment(
        &self,
        message_id: &str,
        filename: &str,
        content_type: &str,
        size: i64,
        path: &str,
        sha256: &str,
    ) -> DbResult<String> {
        let id = utils::new_id();
        self.exec(
            "INSERT INTO attachments (id, message_id, filename, content_type, size_bytes, path, sha256, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![&id, message_id, filename, content_type, size, path, sha256, utils::now()],
        )
        .await?;
        Ok(id)
    }

    /// (path, file name, content type) of a stored attachment.
    pub async fn attachment_file(&self, id: &str) -> DbResult<Option<(String, String, String)>> {
        Ok(self
            .row(
                "SELECT path, filename, content_type FROM attachments WHERE id = ?",
                params![id],
            )
            .await?
            .and_then(|r| {
                Some((
                    get_s(&r, "path")?,
                    get_s(&r, "filename").unwrap_or_else(|| "attachment".into()),
                    get_s(&r, "content_type").unwrap_or_else(|| "application/octet-stream".into()),
                ))
            }))
    }

    pub async fn attachments_for(&self, message_id: &str) -> DbResult<Vec<Json>> {
        self.json_rows(
            "SELECT id, filename, content_type, size_bytes, sha256, created_at FROM attachments WHERE message_id = ? ORDER BY created_at",
            params![message_id],
        )
        .await
    }

    // ---- modem, SIM, USSD, GNSS ------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_health(
        &self,
        modem_id: &str,
        state: &str,
        rssi: Option<i64>,
        percent: Option<i64>,
        registration: Option<&str>,
        operator: Option<&str>,
        network_type: Option<&str>,
    ) -> DbResult<()> {
        self.exec(
            "INSERT INTO modem_health (modem_id, state, rssi_dbm, signal_percent, registration, operator, network_type, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![modem_id, state, rssi, percent, registration, operator, network_type, utils::now()],
        )
        .await?;
        Ok(())
    }

    pub async fn health_history(&self, modem_id: &str, limit: i64) -> DbResult<Vec<Json>> {
        self.json_rows(
            "SELECT state, rssi_dbm, signal_percent, registration, operator, network_type, created_at FROM modem_health WHERE modem_id = ? ORDER BY id DESC LIMIT ?",
            params![modem_id, limit.clamp(1, 1000)],
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_sim_info(
        &self,
        modem_id: &str,
        sim_id: Option<&str>,
        iccid: Option<&str>,
        imsi: Option<&str>,
        msisdn: Option<&str>,
        operator: Option<&str>,
        pin_state: Option<&str>,
        smsc: Option<&str>,
    ) -> DbResult<()> {
        let now = utils::now();
        let n = self
            .exec(
                "UPDATE sim_information SET sim_id = ?, iccid = ?, imsi = ?, msisdn = ?, operator = ?, pin_state = ?, smsc = ?, updated_at = ? WHERE modem_id = ?",
                params![sim_id, iccid, imsi, msisdn, operator, pin_state, smsc, &now, modem_id],
            )
            .await?;
        if n == 0 {
            let r = self
                .exec(
                    "INSERT INTO sim_information (modem_id, sim_id, iccid, imsi, msisdn, operator, pin_state, smsc, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    params![modem_id, sim_id, iccid, imsi, msisdn, operator, pin_state, smsc, &now],
                )
                .await;
            match r {
                Err(e) if !e.is_unique_violation() => return Err(e),
                _ => {}
            }
        }
        Ok(())
    }

    /// SIM profile bound to a modem at its last successful initialisation.
    pub async fn bound_sim_id(&self, modem_id: &str) -> DbResult<Option<String>> {
        Ok(self
            .row(
                "SELECT sim_id FROM sim_information WHERE modem_id = ?",
                params![modem_id],
            )
            .await?
            .and_then(|r| get_s(&r, "sim_id")))
    }

    pub async fn insert_ussd(&self, id: &str, modem_id: &str, code: &str) -> DbResult<()> {
        self.exec(
            "INSERT INTO ussd_sessions (id, modem_id, code, status, created_at) VALUES (?, ?, ?, 'pending', ?)",
            params![id, modem_id, code, utils::now()],
        )
        .await?;
        Ok(())
    }

    pub async fn finish_ussd(
        &self,
        id: &str,
        status: &str,
        response: Option<&str>,
    ) -> DbResult<()> {
        self.exec(
            "UPDATE ussd_sessions SET status = ?, response = ?, completed_at = ? WHERE id = ?",
            params![status, response, utils::now(), id],
        )
        .await?;
        Ok(())
    }

    pub async fn list_ussd(&self, limit: i64, offset: i64) -> DbResult<(Vec<Json>, i64)> {
        let total = self
            .count("SELECT COUNT(*) FROM ussd_sessions", vec![])
            .await?;
        let mut ps = vec![];
        page(&mut ps, limit, offset);
        Ok((
            self.json_rows(
                "SELECT * FROM ussd_sessions ORDER BY created_at DESC LIMIT ? OFFSET ?",
                ps,
            )
            .await?,
            total,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_gnss_fix(
        &self,
        modem_id: &str,
        lat: f64,
        lon: f64,
        alt: Option<f64>,
        speed: Option<f64>,
        heading: Option<f64>,
        satellites: Option<i64>,
        fix_time: Option<String>,
    ) -> DbResult<()> {
        self.exec(
            "INSERT INTO gnss_fixes (modem_id, latitude, longitude, altitude, speed_kmh, heading, satellites, fix_time, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![modem_id, lat, lon, alt, speed, heading, satellites, fix_time, utils::now()],
        )
        .await?;
        Ok(())
    }

    pub async fn gnss_history(&self, limit: i64) -> DbResult<Vec<Json>> {
        self.json_rows(
            "SELECT modem_id, latitude, longitude, altitude, speed_kmh, heading, satellites, fix_time, created_at FROM gnss_fixes ORDER BY id DESC LIMIT ?",
            params![limit.clamp(1, 1000)],
        )
        .await
    }

    // ---- audit, automation, key/value state ------------------------------

    pub async fn audit(
        &self,
        actor: Option<&str>,
        action: &str,
        target: Option<&str>,
        detail: Option<&str>,
        request_id: Option<&str>,
    ) -> DbResult<()> {
        self.exec(
            "INSERT INTO audit_events (actor, action, target, detail, request_id, created_at) VALUES (?, ?, ?, ?, ?, ?)",
            params![actor, action, target, detail, request_id, utils::now()],
        )
        .await?;
        Ok(())
    }

    pub async fn list_audit(&self, limit: i64, offset: i64) -> DbResult<(Vec<Json>, i64)> {
        let total = self
            .count("SELECT COUNT(*) FROM audit_events", vec![])
            .await?;
        let mut ps = vec![];
        page(&mut ps, limit, offset);
        Ok((
            self.json_rows(
                "SELECT * FROM audit_events ORDER BY id DESC LIMIT ? OFFSET ?",
                ps,
            )
            .await?,
            total,
        ))
    }

    pub async fn log_automation(&self, rule: &str, sender: &str, message_id: &str) -> DbResult<()> {
        self.exec(
            "INSERT INTO automation_log (rule_name, sender, message_id, created_at) VALUES (?, ?, ?, ?)",
            params![rule, sender, message_id, utils::now()],
        )
        .await?;
        Ok(())
    }

    /// (rule firings since `since`, firings for this sender since `sender_since`)
    pub async fn automation_counts(
        &self,
        rule: &str,
        sender: &str,
        since: &str,
        sender_since: &str,
    ) -> DbResult<(i64, i64)> {
        let all = self
            .count(
                "SELECT COUNT(*) FROM automation_log WHERE rule_name = ? AND created_at >= ?",
                params![rule, since],
            )
            .await?;
        let by_sender = self
            .count(
                "SELECT COUNT(*) FROM automation_log WHERE rule_name = ? AND sender = ? AND created_at >= ?",
                params![rule, sender, sender_since],
            )
            .await?;
        Ok((all, by_sender))
    }

    pub async fn kv_get(&self, key: &str) -> DbResult<Option<String>> {
        Ok(self
            .row("SELECT v FROM kv_state WHERE k = ?", params![key])
            .await?
            .and_then(|r| get_s(&r, "v")))
    }

    pub async fn kv_set(&self, key: &str, value: &str) -> DbResult<()> {
        let now = utils::now();
        let n = self
            .exec(
                "UPDATE kv_state SET v = ?, updated_at = ? WHERE k = ?",
                params![value, &now, key],
            )
            .await?;
        if n == 0 {
            // MySQL reports zero affected rows for an unchanged value, so a
            // duplicate key here simply means the row already exists.
            match self
                .exec(
                    "INSERT INTO kv_state (k, v, updated_at) VALUES (?, ?, ?)",
                    params![key, value, &now],
                )
                .await
            {
                Err(e) if !e.is_unique_violation() => return Err(e),
                _ => {}
            }
        }
        Ok(())
    }

    // ---- statistics -------------------------------------------------------

    pub async fn stats(&self) -> DbResult<Json> {
        let c = |sql: &'static str| async move { self.count(sql, vec![]).await };
        let received = c("SELECT COUNT(*) FROM messages WHERE direction = 'inbound'").await?;
        let outbound = c("SELECT COUNT(*) FROM messages WHERE direction = 'outbound'").await?;
        let sent = c("SELECT COUNT(*) FROM messages WHERE direction = 'outbound' AND status IN ('submitted', 'delivered', 'delivery_failed')").await?;
        let delivered = c(
            "SELECT COUNT(*) FROM messages WHERE direction = 'outbound' AND status = 'delivered'",
        )
        .await?;
        let delivery_failed = c("SELECT COUNT(*) FROM messages WHERE direction = 'outbound' AND status = 'delivery_failed'").await?;
        let failed = c("SELECT COUNT(*) FROM messages WHERE direction = 'outbound' AND status IN ('failed', 'delivery_failed', 'expired')").await?;
        let unknown =
            c("SELECT COUNT(*) FROM messages WHERE status = 'submission_unknown'").await?;
        let reported = delivered + delivery_failed;
        let rate = if reported > 0 {
            Json::from((delivered as f64 / reported as f64 * 1000.0).round() / 10.0)
        } else {
            Json::Null
        };
        Ok(json!({
            "messages": {
                "received": received,
                "outbound_total": outbound,
                "sent": sent,
                "delivered": delivered,
                "delivery_failed": delivery_failed,
                "failed": failed,
                "submission_unknown": unknown,
                "delivery_success_rate_percent": rate,
            },
            "queue": {
                "pending": self.queue_depth().await?,
            },
            "integrations": {
                "pending": c("SELECT COUNT(*) FROM integration_events WHERE status IN ('pending', 'retrying', 'processing')").await?,
                "delivered": c("SELECT COUNT(*) FROM integration_events WHERE status = 'delivered'").await?,
                "dead": c("SELECT COUNT(*) FROM integration_events WHERE status = 'dead'").await?,
            },
            "calls": { "total": c("SELECT COUNT(*) FROM calls").await? },
        }))
    }

    /// Message counts per time bucket. `bucket_len` is 13 for hourly buckets
    /// and 10 for daily buckets (prefix length of the timestamp).
    pub async fn activity(&self, since: &str, bucket_len: i64) -> DbResult<Vec<Json>> {
        // The length is inlined (it is one of two fixed integers) so that the
        // SELECT and GROUP BY expressions are identical for strict SQL modes.
        let n = if bucket_len >= 13 { 13 } else { 10 };
        self.json_rows(
            &format!("SELECT SUBSTR(created_at, 1, {n}) AS bucket, direction, COUNT(*) AS count FROM messages WHERE created_at >= ? GROUP BY SUBSTR(created_at, 1, {n}), direction ORDER BY bucket"),
            params![since],
        )
        .await
    }

    pub async fn table_counts(&self) -> DbResult<Json> {
        let mut m = Map::new();
        for t in [
            "messages",
            "message_parts",
            "outbound_queue",
            "delivery_reports",
            "events",
            "integration_events",
            "integration_attempts",
            "dead_letter_queue",
            "calls",
            "recordings",
            "attachments",
            "ussd_sessions",
            "audit_events",
        ] {
            m.insert(
                t.into(),
                self.count(&format!("SELECT COUNT(*) FROM {t}"), vec![])
                    .await?
                    .into(),
            );
        }
        Ok(Json::Object(m))
    }

    // ---- retention --------------------------------------------------------

    /// Delete rows older than the given cut-offs. `None` keeps the data.
    /// Returns file paths (recordings, attachments) that should be removed.
    pub async fn apply_retention(
        &self,
        messages_before: Option<String>,
        recordings_before: Option<String>,
        attachments_before: Option<String>,
        events_before: Option<String>,
        audit_before: Option<String>,
    ) -> DbResult<(Json, Vec<String>)> {
        let mut report = Map::new();
        let mut files = Vec::new();
        if let Some(t) = messages_before {
            // Never remove messages that are still waiting in the queue.
            let active = "SELECT message_id FROM outbound_queue WHERE status IN ('queued', 'scheduled', 'processing', 'retrying')";
            for (name, sql) in [
                ("message_parts", format!("DELETE FROM message_parts WHERE message_id IN (SELECT id FROM messages WHERE created_at < ?) AND message_id NOT IN ({active})")),
                ("message_status_history", format!("DELETE FROM message_status_history WHERE message_id IN (SELECT id FROM messages WHERE created_at < ?) AND message_id NOT IN ({active})")),
                ("delivery_reports", "DELETE FROM delivery_reports WHERE received_at < ?".to_string()),
                ("outbound_queue", "DELETE FROM outbound_queue WHERE created_at < ? AND status NOT IN ('queued', 'scheduled', 'processing', 'retrying')".to_string()),
                ("messages", "DELETE FROM messages WHERE created_at < ? AND id NOT IN (SELECT message_id FROM outbound_queue)".to_string()),
            ] {
                report.insert(name.into(), self.exec(&sql, params![&t]).await?.into());
            }
        }
        if let Some(t) = recordings_before {
            for r in self
                .rows(
                    "SELECT path FROM recordings WHERE created_at < ?",
                    params![&t],
                )
                .await?
            {
                files.extend(get_s(&r, "path"));
            }
            report.insert(
                "recordings".into(),
                self.exec("DELETE FROM recordings WHERE created_at < ?", params![&t])
                    .await?
                    .into(),
            );
        }
        if let Some(t) = attachments_before {
            for r in self
                .rows(
                    "SELECT path FROM attachments WHERE created_at < ?",
                    params![&t],
                )
                .await?
            {
                files.extend(get_s(&r, "path"));
            }
            report.insert(
                "attachments".into(),
                self.exec("DELETE FROM attachments WHERE created_at < ?", params![&t])
                    .await?
                    .into(),
            );
        }
        if let Some(t) = events_before {
            for (name, sql) in [
                ("integration_attempts", "DELETE FROM integration_attempts WHERE created_at < ?"),
                ("integration_events", "DELETE FROM integration_events WHERE created_at < ? AND status IN ('delivered', 'dead')"),
                ("dead_letter_queue", "DELETE FROM dead_letter_queue WHERE created_at < ?"),
                ("events", "DELETE FROM events WHERE created_at < ? AND id NOT IN (SELECT event_id FROM integration_events)"),
                ("modem_health", "DELETE FROM modem_health WHERE created_at < ?"),
                ("automation_log", "DELETE FROM automation_log WHERE created_at < ?"),
            ] {
                report.insert(name.into(), self.exec(sql, params![&t]).await?.into());
            }
        }
        if let Some(t) = audit_before {
            report.insert(
                "audit_events".into(),
                self.exec("DELETE FROM audit_events WHERE created_at < ?", params![&t])
                    .await?
                    .into(),
            );
        }
        Ok((Json::Object(report), files))
    }
}

impl Db {
    /// Contacts saved through the dashboard or API, ordered by name.
    pub async fn saved_contacts(&self) -> DbResult<Vec<crate::config::Contact>> {
        Ok(self
            .rows(
                "SELECT id, name, number, note FROM contacts ORDER BY name, number",
                vec![],
            )
            .await?
            .iter()
            .map(|r| crate::config::Contact {
                id: get_s(r, "id").unwrap_or_default(),
                name: get_s(r, "name").unwrap_or_default(),
                number: get_s(r, "number").unwrap_or_default(),
                note: get_s(r, "note").unwrap_or_default(),
                source: "saved".into(),
            })
            .collect())
    }

    pub async fn insert_contact(&self, name: &str, number: &str, note: &str) -> DbResult<String> {
        let id = utils::new_id();
        let now = utils::now();
        self.exec(
            "INSERT INTO contacts (id, name, number, note, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
            params![&id, name, number, note, &now, &now],
        )
        .await?;
        Ok(id)
    }

    /// Returns false when no contact has this id.
    pub async fn update_contact(
        &self,
        id: &str,
        name: &str,
        number: &str,
        note: &str,
    ) -> DbResult<bool> {
        // The existence check keeps "unchanged" apart from "missing": MySQL
        // reports zero affected rows for an update that changes nothing.
        if self
            .count("SELECT COUNT(*) FROM contacts WHERE id = ?", params![id])
            .await?
            == 0
        {
            return Ok(false);
        }
        self.exec(
            "UPDATE contacts SET name = ?, number = ?, note = ?, updated_at = ? WHERE id = ?",
            params![name, number, note, utils::now(), id],
        )
        .await?;
        Ok(true)
    }

    pub async fn delete_contact(&self, id: &str) -> DbResult<bool> {
        Ok(self
            .exec("DELETE FROM contacts WHERE id = ?", params![id])
            .await?
            == 1)
    }
}

impl Db {
    /// Record a detected inbound attack (honeypot classification).
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_security_event(
        &self,
        m: &Message,
        category: &str,
        severity: &str,
        confidence: i64,
        detail: &str,
    ) -> DbResult<()> {
        self.exec(
            "INSERT INTO security_events (id, message_id, modem_id, sim_id, sender, recipient, category, severity, confidence, detail, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                utils::new_id(),
                &m.id,
                &m.modem_id,
                &m.sim_id,
                &m.sender,
                &m.recipient,
                category,
                severity,
                confidence,
                detail,
                utils::now()
            ],
        )
        .await?;
        Ok(())
    }

    /// List detected attacks, newest first, with optional filters.
    #[allow(clippy::too_many_arguments)]
    pub async fn security_events(
        &self,
        category: Option<&str>,
        modem: Option<&str>,
        number: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> DbResult<(Vec<Json>, i64)> {
        let mut wh = String::from("1 = 1");
        let mut ps: Vec<P> = Vec::new();
        for (clause, value) in [
            ("category = ?", category),
            ("modem_id = ?", modem),
            ("recipient = ?", number),
            ("created_at >= ?", from),
            ("created_at <= ?", to),
        ] {
            if let Some(v) = value.filter(|v| !v.is_empty()) {
                wh.push_str(" AND ");
                wh.push_str(clause);
                ps.push(v.into());
            }
        }
        let total = self
            .count(
                &format!("SELECT COUNT(*) FROM security_events WHERE {wh}"),
                ps.clone(),
            )
            .await?;
        ps.push(limit.clamp(1, 1000).into());
        ps.push(offset.max(0).into());
        // Columns are qualified and joined to the message so a collector gets
        // the attacker, the raw PDU and the decoded text in one pull.
        let wh = wh
            .replace("category = ", "e.category = ")
            .replace("modem_id = ", "e.modem_id = ")
            .replace("recipient = ", "e.recipient = ")
            .replace("created_at >= ", "e.created_at >= ")
            .replace("created_at <= ", "e.created_at <= ");
        let sql = format!(
            "SELECT e.id, e.message_id, e.modem_id, e.sim_id, e.sender, e.recipient, e.category, e.severity, e.confidence, e.detail, e.created_at, m.message_type, m.encoding, m.protocol_id, m.dst_port, m.raw_pdu, m.body, m.network_timestamp, m.metadata FROM security_events e LEFT JOIN messages m ON m.id = e.message_id WHERE {wh} ORDER BY e.created_at DESC LIMIT ? OFFSET ?"
        );
        Ok((self.json_rows(&sql, ps).await?, total))
    }

    /// Attack statistics since `from`: totals and counts by category,
    /// severity and targeted number.
    pub async fn security_stats(&self, from: &str) -> DbResult<Json> {
        let total = self
            .count(
                "SELECT COUNT(*) FROM security_events WHERE created_at >= ?",
                params![from],
            )
            .await?;
        let by_category = self.json_rows(
            "SELECT category AS k, COUNT(*) AS count, AVG(confidence) AS avg_confidence, MAX(confidence) AS max_confidence, MAX(created_at) AS last_seen FROM security_events WHERE created_at >= ? GROUP BY category ORDER BY count DESC",
            params![from],
        ).await?;
        let by_severity = self.json_rows(
            "SELECT severity AS k, COUNT(*) AS count FROM security_events WHERE created_at >= ? GROUP BY severity ORDER BY count DESC",
            params![from],
        ).await?;
        // Per targeted (honeypot) number: volume, peak confidence, distinct
        // attackers and when it was last hit.
        let by_number = self.json_rows(
            "SELECT recipient AS k, COUNT(*) AS count, MAX(confidence) AS max_confidence, COUNT(DISTINCT sender) AS distinct_senders, MAX(created_at) AS last_seen FROM security_events WHERE created_at >= ? GROUP BY recipient ORDER BY count DESC",
            params![from],
        ).await?;
        // Each number broken down by attack category (ready-made for charts).
        let number_category = self.json_rows(
            "SELECT recipient AS number, category, COUNT(*) AS count FROM security_events WHERE created_at >= ? GROUP BY recipient, category ORDER BY recipient, count DESC",
            params![from],
        ).await?;
        // Daily counts per category (SUBSTR keeps it portable across engines).
        let timeline = self.json_rows(
            "SELECT SUBSTR(created_at, 1, 10) AS day, category, COUNT(*) AS count FROM security_events WHERE created_at >= ? GROUP BY day, category ORDER BY day",
            params![from],
        ).await?;
        let overall = self
            .json_rows(
                "SELECT AVG(confidence) AS avg_confidence, MAX(confidence) AS max_confidence, COUNT(DISTINCT sender) AS distinct_senders, COUNT(DISTINCT recipient) AS targeted_numbers, MIN(created_at) AS first_seen, MAX(created_at) AS last_seen FROM security_events WHERE created_at >= ?",
                params![from],
            )
            .await?;
        let o = overall.into_iter().next().unwrap_or(Json::Null);
        Ok(serde_json::json!({
            "total": total,
            "avg_confidence": o["avg_confidence"],
            "max_confidence": o["max_confidence"],
            "distinct_senders": o["distinct_senders"],
            "targeted_numbers": o["targeted_numbers"],
            "first_seen": o["first_seen"],
            "last_seen": o["last_seen"],
            "by_category": by_category,
            "by_severity": by_severity,
            "by_number": by_number,
            "number_category": number_category,
            "timeline": timeline,
        }))
    }
}
