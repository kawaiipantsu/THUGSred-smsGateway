//! Message, part and status-history persistence.

use super::*;
use crate::params;

pub(crate) const MSG_COLS: &str = "id, direction, message_type, instance_id, modem_id, sim_id, sender, recipient, original_address, normalized_number, body, raw_pdu, payload, encoding, message_class, protocol_id, data_coding, smsc, concat_ref, concat_total, parts, src_port, dst_port, network_timestamp, received_at, submitted_at, delivered_at, status, delivery_status, error, retry_count, metadata, integration_status, correlation_id, idempotency_key, fingerprint, modem_ref, delivery_report, created_at, updated_at";

pub(crate) const PART_COLS: &str = "id, message_id, direction, modem_id, address, concat_ref, concat_total, sequence, wide_ref, encoding, shifts, pdu, payload, tpdu_length, fingerprint, status, modem_ref, error, submitted_at, delivered_at, created_at";

pub(crate) fn msg_from_row(r: &AnyRow) -> Message {
    Message {
        id: get_s(r, "id").unwrap_or_default(),
        direction: get_s(r, "direction").unwrap_or_default(),
        message_type: get_s(r, "message_type").unwrap_or_default(),
        instance_id: get_s(r, "instance_id").unwrap_or_default(),
        modem_id: get_s(r, "modem_id"),
        sim_id: get_s(r, "sim_id"),
        sender: get_s(r, "sender"),
        recipient: get_s(r, "recipient"),
        original_address: get_s(r, "original_address"),
        normalized_number: get_s(r, "normalized_number"),
        body: get_s(r, "body"),
        raw_pdu: get_s(r, "raw_pdu"),
        payload: get_b(r, "payload"),
        encoding: get_s(r, "encoding"),
        message_class: get_i(r, "message_class"),
        protocol_id: get_i(r, "protocol_id"),
        data_coding: get_i(r, "data_coding"),
        smsc: get_s(r, "smsc"),
        concat_ref: get_i(r, "concat_ref"),
        concat_total: get_i(r, "concat_total"),
        parts: get_i(r, "parts").unwrap_or(1),
        src_port: get_i(r, "src_port"),
        dst_port: get_i(r, "dst_port"),
        network_timestamp: get_s(r, "network_timestamp"),
        received_at: get_s(r, "received_at"),
        submitted_at: get_s(r, "submitted_at"),
        delivered_at: get_s(r, "delivered_at"),
        status: get_s(r, "status").unwrap_or_default(),
        delivery_status: get_s(r, "delivery_status"),
        error: get_s(r, "error"),
        retry_count: get_i(r, "retry_count").unwrap_or(0),
        metadata: get_s(r, "metadata").and_then(|s| serde_json::from_str(&s).ok()),
        integration_status: get_s(r, "integration_status"),
        correlation_id: get_s(r, "correlation_id"),
        idempotency_key: get_s(r, "idempotency_key"),
        fingerprint: get_s(r, "fingerprint"),
        modem_ref: get_i(r, "modem_ref"),
        delivery_report: get_i(r, "delivery_report").unwrap_or(0) != 0,
        created_at: get_s(r, "created_at").unwrap_or_default(),
        updated_at: get_s(r, "updated_at").unwrap_or_default(),
    }
}

pub(crate) fn part_from_row(r: &AnyRow) -> Part {
    Part {
        id: get_s(r, "id").unwrap_or_default(),
        message_id: get_s(r, "message_id"),
        direction: get_s(r, "direction").unwrap_or_default(),
        modem_id: get_s(r, "modem_id"),
        address: get_s(r, "address"),
        concat_ref: get_i(r, "concat_ref"),
        concat_total: get_i(r, "concat_total"),
        sequence: get_i(r, "sequence").unwrap_or(1),
        wide_ref: get_i(r, "wide_ref").unwrap_or(0) != 0,
        encoding: get_s(r, "encoding"),
        shifts: get_i(r, "shifts").unwrap_or(0),
        pdu: get_s(r, "pdu"),
        payload: get_b(r, "payload"),
        tpdu_length: get_i(r, "tpdu_length"),
        fingerprint: get_s(r, "fingerprint"),
        status: get_s(r, "status").unwrap_or_default(),
        modem_ref: get_i(r, "modem_ref"),
        error: get_s(r, "error"),
        submitted_at: get_s(r, "submitted_at"),
        delivered_at: get_s(r, "delivered_at"),
        created_at: get_s(r, "created_at").unwrap_or_default(),
    }
}

fn msg_params(m: &Message) -> Vec<P> {
    params![
        &m.id,
        &m.direction,
        &m.message_type,
        &m.instance_id,
        &m.modem_id,
        &m.sim_id,
        &m.sender,
        &m.recipient,
        &m.original_address,
        &m.normalized_number,
        &m.body,
        &m.raw_pdu,
        m.payload.clone(),
        &m.encoding,
        m.message_class,
        m.protocol_id,
        m.data_coding,
        &m.smsc,
        m.concat_ref,
        m.concat_total,
        m.parts,
        m.src_port,
        m.dst_port,
        &m.network_timestamp,
        &m.received_at,
        &m.submitted_at,
        &m.delivered_at,
        &m.status,
        &m.delivery_status,
        &m.error,
        m.retry_count,
        m.metadata.as_ref().map(|j| j.to_string()),
        &m.integration_status,
        &m.correlation_id,
        &m.idempotency_key,
        &m.fingerprint,
        m.modem_ref,
        m.delivery_report,
        &m.created_at,
        &m.updated_at
    ]
}

fn part_params(p: &Part) -> Vec<P> {
    params![
        &p.id,
        &p.message_id,
        &p.direction,
        &p.modem_id,
        &p.address,
        p.concat_ref,
        p.concat_total,
        p.sequence,
        p.wide_ref,
        &p.encoding,
        p.shifts,
        &p.pdu,
        p.payload.clone(),
        p.tpdu_length,
        &p.fingerprint,
        &p.status,
        p.modem_ref,
        &p.error,
        &p.submitted_at,
        &p.delivered_at,
        &p.created_at
    ]
}

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(", ")
}

/// Escape a user string for use inside a `LIKE ? ESCAPE '!'` pattern.
pub(crate) fn like_pattern(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('%');
    for c in s.chars() {
        if matches!(c, '%' | '_' | '!') {
            out.push('!');
        }
        out.push(c);
    }
    out.push('%');
    out
}

impl Db {
    pub(crate) async fn tx_insert_message(
        tx: &mut sqlx::Transaction<'_, Any>,
        m: &Message,
    ) -> DbResult<()> {
        let sql = format!(
            "INSERT INTO messages ({MSG_COLS}) VALUES ({})",
            placeholders(40)
        );
        Self::tx_exec(tx, &sql, msg_params(m)).await?;
        Self::tx_exec(
            tx,
            "INSERT INTO message_status_history (message_id, status, detail, created_at) VALUES (?, ?, ?, ?)",
            params![&m.id, &m.status, &m.error, &m.created_at],
        )
        .await?;
        Ok(())
    }

    pub(crate) async fn tx_insert_part(
        tx: &mut sqlx::Transaction<'_, Any>,
        p: &Part,
    ) -> DbResult<()> {
        let sql = format!(
            "INSERT INTO message_parts ({PART_COLS}) VALUES ({})",
            placeholders(21)
        );
        Self::tx_exec(tx, &sql, part_params(p)).await?;
        Ok(())
    }

    /// Persist a message together with its parts atomically.
    pub async fn insert_message(&self, m: &Message, parts: &[Part]) -> DbResult<()> {
        let mut tx = self.begin().await?;
        Self::tx_insert_message(&mut tx, m).await?;
        for p in parts {
            Self::tx_insert_part(&mut tx, p).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Store one pending segment of an inbound concatenated message.
    pub async fn insert_part(&self, p: &Part) -> DbResult<()> {
        let sql = format!(
            "INSERT INTO message_parts ({PART_COLS}) VALUES ({})",
            placeholders(21)
        );
        self.exec(&sql, part_params(p)).await?;
        Ok(())
    }

    /// Persist an assembled multipart message and attach its segments.
    pub async fn insert_assembled(&self, m: &Message, part_ids: &[String]) -> DbResult<()> {
        let mut tx = self.begin().await?;
        Self::tx_insert_message(&mut tx, m).await?;
        for id in part_ids {
            Self::tx_exec(
                &mut tx,
                "UPDATE message_parts SET message_id = ?, status = 'assembled' WHERE id = ?",
                params![&m.id, id],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn get_message(&self, id: &str) -> DbResult<Option<Message>> {
        let sql = format!("SELECT {MSG_COLS} FROM messages WHERE id = ?");
        Ok(self
            .row(&sql, params![id])
            .await?
            .as_ref()
            .map(msg_from_row))
    }

    pub async fn find_by_idempotency_key(&self, key: &str) -> DbResult<Option<Message>> {
        let sql = format!("SELECT {MSG_COLS} FROM messages WHERE idempotency_key = ?");
        Ok(self
            .row(&sql, params![key])
            .await?
            .as_ref()
            .map(msg_from_row))
    }

    /// Whether a PDU fingerprint has already been ingested.
    pub async fn fingerprint_known(&self, fp: &str) -> DbResult<bool> {
        for table in ["messages", "message_parts", "delivery_reports"] {
            let sql = format!("SELECT COUNT(*) FROM {table} WHERE fingerprint = ?");
            if self.count(&sql, params![fp]).await? > 0 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub async fn search_messages(&self, q: &MessageQuery) -> DbResult<(Vec<Message>, i64)> {
        let mut wh = vec!["1 = 1".to_string()];
        let mut ps: Vec<P> = Vec::new();
        let eq = |col: &str, v: &Option<String>, wh: &mut Vec<String>, ps: &mut Vec<P>| {
            if let Some(v) = v.as_ref().filter(|v| !v.is_empty()) {
                wh.push(format!("{col} = ?"));
                ps.push(v.into());
            }
        };
        eq("direction", &q.direction, &mut wh, &mut ps);
        eq("message_type", &q.message_type, &mut wh, &mut ps);
        eq("status", &q.status, &mut wh, &mut ps);
        eq("modem_id", &q.modem_id, &mut wh, &mut ps);
        for (col, v) in [("sender", &q.sender), ("recipient", &q.recipient)] {
            if let Some(v) = v.as_ref().filter(|v| !v.is_empty()) {
                wh.push(format!("{col} LIKE ? ESCAPE '!'"));
                ps.push(like_pattern(v).into());
            }
        }
        if let Some(s) = q.search.as_ref().filter(|v| !v.is_empty()) {
            wh.push("(body LIKE ? ESCAPE '!' OR sender LIKE ? ESCAPE '!' OR recipient LIKE ? ESCAPE '!')".into());
            for _ in 0..3 {
                ps.push(like_pattern(s).into());
            }
        }
        if let Some(f) = q.from.as_ref().filter(|v| !v.is_empty()) {
            wh.push("created_at >= ?".into());
            ps.push(f.into());
        }
        if let Some(t) = q.to.as_ref().filter(|v| !v.is_empty()) {
            wh.push("created_at <= ?".into());
            ps.push(t.into());
        }
        let wh = wh.join(" AND ");
        let total = self
            .count(
                &format!("SELECT COUNT(*) FROM messages WHERE {wh}"),
                ps.clone(),
            )
            .await?;
        let order = if q.ascending { "ASC" } else { "DESC" };
        ps.push(q.limit.clamp(1, 10_000).into());
        ps.push(q.offset.max(0).into());
        let sql = format!(
            "SELECT {MSG_COLS} FROM messages WHERE {wh} ORDER BY created_at {order}, id {order} LIMIT ? OFFSET ?"
        );
        let rows = self.rows(&sql, ps).await?;
        Ok((rows.iter().map(msg_from_row).collect(), total))
    }

    /// Delete a message and everything attached to it. Returns false if unknown.
    pub async fn delete_message(&self, id: &str) -> DbResult<bool> {
        let mut tx = self.begin().await?;
        let n = Self::tx_exec(&mut tx, "DELETE FROM messages WHERE id = ?", params![id]).await?;
        for sql in [
            "DELETE FROM message_parts WHERE message_id = ?",
            "DELETE FROM message_status_history WHERE message_id = ?",
            "DELETE FROM outbound_queue WHERE message_id = ?",
            "DELETE FROM delivery_reports WHERE message_id = ?",
        ] {
            Self::tx_exec(&mut tx, sql, params![id]).await?;
        }
        tx.commit().await?;
        Ok(n > 0)
    }

    pub async fn add_history(&self, id: &str, status: &str, detail: Option<&str>) -> DbResult<()> {
        self.exec(
            "INSERT INTO message_status_history (message_id, status, detail, created_at) VALUES (?, ?, ?, ?)",
            params![id, status, detail, utils::now()],
        )
        .await?;
        Ok(())
    }

    pub async fn history(&self, id: &str) -> DbResult<Vec<Json>> {
        self.json_rows(
            "SELECT status, detail, created_at FROM message_status_history WHERE message_id = ? ORDER BY id",
            params![id],
        )
        .await
    }

    pub async fn set_integration_status(&self, id: &str, status: &str) -> DbResult<()> {
        self.exec(
            "UPDATE messages SET integration_status = ? WHERE id = ?",
            params![status, id],
        )
        .await?;
        Ok(())
    }

    pub async fn parts_for_message(&self, id: &str) -> DbResult<Vec<Part>> {
        let sql = format!("SELECT {PART_COLS} FROM message_parts WHERE message_id = ? ORDER BY sequence, created_at");
        Ok(self
            .rows(&sql, params![id])
            .await?
            .iter()
            .map(part_from_row)
            .collect())
    }

    /// Pending inbound segments that belong to one concatenation group.
    pub async fn pending_group(
        &self,
        modem_id: &str,
        address: &str,
        concat_ref: i64,
        total: i64,
        wide: bool,
    ) -> DbResult<Vec<Part>> {
        let sql = format!(
            "SELECT {PART_COLS} FROM message_parts WHERE direction = 'inbound' AND status = 'pending' AND modem_id = ? AND address = ? AND concat_ref = ? AND concat_total = ? AND wide_ref = ? ORDER BY sequence, created_at"
        );
        Ok(self
            .rows(&sql, params![modem_id, address, concat_ref, total, wide])
            .await?
            .iter()
            .map(part_from_row)
            .collect())
    }

    /// Pending inbound segments older than `cutoff`.
    pub async fn stale_pending_parts(&self, cutoff: &str) -> DbResult<Vec<Part>> {
        let sql = format!(
            "SELECT {PART_COLS} FROM message_parts WHERE direction = 'inbound' AND status = 'pending' AND created_at < ? ORDER BY created_at"
        );
        Ok(self
            .rows(&sql, params![cutoff])
            .await?
            .iter()
            .map(part_from_row)
            .collect())
    }

    pub async fn update_part(
        &self,
        id: &str,
        status: &str,
        modem_ref: Option<i64>,
        error: Option<&str>,
    ) -> DbResult<()> {
        let now = utils::now();
        let (submitted, delivered) = match status {
            "submitted" => (Some(now.clone()), None),
            "delivered" => (None, Some(now)),
            _ => (None, None),
        };
        self.exec(
            "UPDATE message_parts SET status = ?, modem_ref = COALESCE(?, modem_ref), error = ?, submitted_at = COALESCE(?, submitted_at), delivered_at = COALESCE(?, delivered_at) WHERE id = ?",
            params![status, modem_ref, error, submitted, delivered, id],
        )
        .await?;
        Ok(())
    }

    /// Set the modem that actually carries the parts of an outbound message.
    pub async fn assign_modem(
        &self,
        message_id: &str,
        modem_id: &str,
        sim_id: Option<&str>,
        sender: Option<&str>,
    ) -> DbResult<()> {
        self.exec(
            "UPDATE messages SET modem_id = ?, sim_id = ?, sender = COALESCE(?, sender) WHERE id = ?",
            params![modem_id, sim_id, sender, message_id],
        )
        .await?;
        self.exec(
            "UPDATE message_parts SET modem_id = ? WHERE message_id = ?",
            params![modem_id, message_id],
        )
        .await?;
        Ok(())
    }
}
