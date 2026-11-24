//! Durable outbound queue, delivery-report correlation and quota counters.

use super::messages::{part_from_row, PART_COLS};
use super::*;
use crate::params;

const QUEUE_COLS: &str = "message_id, status, priority, modem_id, claimed_by, not_before, expires_at, attempts, next_attempt_at, last_error, created_at, updated_at";

/// States from which the sender may pick a message up.
const CLAIMABLE: &str = "'queued', 'retrying', 'scheduled'";

fn queue_from_row(r: &AnyRow) -> QueueItem {
    QueueItem {
        message_id: get_s(r, "message_id").unwrap_or_default(),
        status: get_s(r, "status").unwrap_or_default(),
        priority: get_i(r, "priority").unwrap_or(0),
        modem_id: get_s(r, "modem_id"),
        claimed_by: get_s(r, "claimed_by"),
        not_before: get_s(r, "not_before"),
        expires_at: get_s(r, "expires_at"),
        attempts: get_i(r, "attempts").unwrap_or(0),
        next_attempt_at: get_s(r, "next_attempt_at"),
        last_error: get_s(r, "last_error"),
        created_at: get_s(r, "created_at").unwrap_or_default(),
        updated_at: get_s(r, "updated_at").unwrap_or_default(),
    }
}

impl Db {
    /// Persist an outbound message, its parts and its queue entry atomically.
    pub async fn enqueue(&self, m: &Message, parts: &[Part], q: &QueueItem) -> DbResult<()> {
        let mut tx = self.begin().await?;
        Self::tx_insert_message(&mut tx, m).await?;
        for p in parts {
            Self::tx_insert_part(&mut tx, p).await?;
        }
        Self::tx_exec(
            &mut tx,
            &format!("INSERT INTO outbound_queue ({QUEUE_COLS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"),
            params![
                &q.message_id, &q.status, q.priority, &q.modem_id, &q.claimed_by, &q.not_before,
                &q.expires_at, q.attempts, &q.next_attempt_at, &q.last_error, &q.created_at,
                &q.updated_at
            ],
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn queue_item(&self, id: &str) -> DbResult<Option<QueueItem>> {
        let sql = format!("SELECT {QUEUE_COLS} FROM outbound_queue WHERE message_id = ?");
        Ok(self
            .row(&sql, params![id])
            .await?
            .as_ref()
            .map(queue_from_row))
    }

    pub async fn list_queue(
        &self,
        status: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> DbResult<(Vec<Json>, i64)> {
        let (wh, mut ps): (String, Vec<P>) = match status {
            Some("all") => ("1 = 1".into(), vec![]),
            Some(s) if !s.is_empty() => ("q.status = ?".into(), params![s]),
            _ => (
                "q.status IN ('queued', 'scheduled', 'processing', 'retrying', 'submission_unknown')".into(),
                vec![],
            ),
        };
        let total = self
            .count(
                &format!("SELECT COUNT(*) FROM outbound_queue q WHERE {wh}"),
                ps.clone(),
            )
            .await?;
        ps.push(limit.clamp(1, 1000).into());
        ps.push(offset.max(0).into());
        let sql = format!(
            "SELECT q.message_id, q.status, q.priority, q.modem_id, q.claimed_by, q.not_before, q.expires_at, q.attempts, q.next_attempt_at, q.last_error, q.created_at, q.updated_at, m.recipient, m.parts, m.encoding FROM outbound_queue q JOIN messages m ON m.id = q.message_id WHERE {wh} ORDER BY q.created_at DESC LIMIT ? OFFSET ?"
        );
        Ok((self.json_rows(&sql, ps).await?, total))
    }

    /// Number of messages waiting to be sent.
    pub async fn queue_depth(&self) -> DbResult<i64> {
        self.count(
            "SELECT COUNT(*) FROM outbound_queue WHERE status IN ('queued', 'scheduled', 'processing', 'retrying')",
            vec![],
        )
        .await
    }

    /// Atomically claim the next due message for a modem worker.
    ///
    /// `take_unassigned` lets the worker also take messages that do not name
    /// a modem. Ordering is priority first, then age.
    pub async fn claim_next(
        &self,
        modem_id: &str,
        take_unassigned: bool,
    ) -> DbResult<Option<QueueItem>> {
        let now = utils::now();
        let modem_clause = if take_unassigned {
            "(modem_id = ? OR modem_id IS NULL)"
        } else {
            "modem_id = ?"
        };
        let sql = format!(
            "SELECT {QUEUE_COLS} FROM outbound_queue WHERE status IN ({CLAIMABLE}) AND (not_before IS NULL OR not_before <= ?) AND (next_attempt_at IS NULL OR next_attempt_at <= ?) AND {modem_clause} ORDER BY priority DESC, created_at ASC LIMIT 5"
        );
        let rows = self.rows(&sql, params![&now, &now, modem_id]).await?;
        for r in rows.iter().map(queue_from_row) {
            let n = self
                .exec(
                    &format!("UPDATE outbound_queue SET status = 'processing', claimed_by = ?, updated_at = ? WHERE message_id = ? AND status IN ({CLAIMABLE})"),
                    params![modem_id, &now, &r.message_id],
                )
                .await?;
            if n == 1 {
                self.exec(
                    "UPDATE messages SET status = 'processing', updated_at = ? WHERE id = ?",
                    params![&now, &r.message_id],
                )
                .await?;
                return Ok(Some(QueueItem {
                    status: "processing".into(),
                    claimed_by: Some(modem_id.to_string()),
                    ..r
                }));
            }
        }
        Ok(None)
    }

    /// Put a claimed message back without counting an attempt (for example
    /// when a rate limit or quota defers it).
    pub async fn release(&self, id: &str, not_before_next: Option<String>) -> DbResult<()> {
        let now = utils::now();
        self.exec(
            "UPDATE outbound_queue SET status = 'queued', claimed_by = NULL, next_attempt_at = ?, updated_at = ? WHERE message_id = ? AND status = 'processing'",
            params![not_before_next, &now, id],
        )
        .await?;
        self.exec(
            "UPDATE messages SET status = 'queued', updated_at = ? WHERE id = ? AND status = 'processing'",
            params![&now, id],
        )
        .await?;
        Ok(())
    }

    /// Move a message to a new state, recording history. `attempt` increments
    /// the attempt counter; `next_attempt_at` schedules a retry.
    pub async fn set_message_state(
        &self,
        id: &str,
        status: &str,
        error: Option<&str>,
        attempt: bool,
        next_attempt_at: Option<String>,
    ) -> DbResult<()> {
        let now = utils::now();
        let mut tx = self.begin().await?;
        Self::tx_exec(
            &mut tx,
            "INSERT INTO message_status_history (message_id, status, detail, created_at) VALUES (?, ?, ?, ?)",
            params![id, status, error, &now],
        )
        .await?;
        let inc = attempt as i64;
        Self::tx_exec(
            &mut tx,
            "UPDATE outbound_queue SET status = ?, last_error = ?, attempts = attempts + ?, next_attempt_at = ?, updated_at = ? WHERE message_id = ?",
            params![status, error, inc, next_attempt_at, &now, id],
        )
        .await?;
        let submitted = (status == "submitted").then(|| now.clone());
        let delivered = (status == "delivered").then(|| now.clone());
        let delivery_status = match status {
            "delivered" => Some("delivered"),
            "delivery_failed" => Some("failed"),
            "submitted" => Some("pending"),
            _ => None,
        };
        Self::tx_exec(
            &mut tx,
            "UPDATE messages SET status = ?, error = ?, retry_count = retry_count + ?, submitted_at = COALESCE(?, submitted_at), delivered_at = COALESCE(?, delivered_at), delivery_status = COALESCE(?, delivery_status), updated_at = ? WHERE id = ?",
            params![status, error, inc, submitted, delivered, delivery_status, &now, id],
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn set_modem_ref(&self, id: &str, modem_ref: i64) -> DbResult<()> {
        self.exec(
            "UPDATE messages SET modem_ref = ? WHERE id = ?",
            params![modem_ref, id],
        )
        .await?;
        Ok(())
    }

    /// Cancel a message that has not been handed to a modem yet.
    pub async fn cancel_queued(&self, id: &str) -> DbResult<bool> {
        let n = self
            .exec(
                &format!("UPDATE outbound_queue SET status = 'cancelled', updated_at = ? WHERE message_id = ? AND status IN ({CLAIMABLE})"),
                params![utils::now(), id],
            )
            .await?;
        if n == 1 {
            self.exec(
                "UPDATE messages SET status = 'cancelled', updated_at = ? WHERE id = ?",
                params![utils::now(), id],
            )
            .await?;
            self.add_history(id, "cancelled", Some("cancelled before transmission"))
                .await?;
        }
        Ok(n == 1)
    }

    /// Requeue a message after an operator decision. Parts that were already
    /// accepted by the network are not sent again.
    pub async fn retry_message(&self, id: &str, expires_at: Option<String>) -> DbResult<bool> {
        let now = utils::now();
        let n = self
            .exec(
                "UPDATE outbound_queue SET status = 'queued', attempts = 0, next_attempt_at = NULL, not_before = NULL, last_error = NULL, claimed_by = NULL, expires_at = ?, updated_at = ? WHERE message_id = ? AND status IN ('failed', 'expired', 'cancelled', 'submission_unknown', 'delivery_failed', 'retrying')",
                params![expires_at, &now, id],
            )
            .await?;
        if n == 1 {
            self.exec(
                "UPDATE message_parts SET status = 'queued', error = NULL WHERE message_id = ? AND status IN ('failed', 'unknown', 'sending')",
                params![id],
            )
            .await?;
            // A delivery failure means every part must be transmitted again.
            self.exec(
                "UPDATE message_parts SET status = 'queued', error = NULL, modem_ref = NULL WHERE message_id = ? AND (SELECT COUNT(*) FROM messages WHERE id = ? AND status = 'delivery_failed') > 0",
                params![id, id],
            )
            .await?;
            self.exec(
                "UPDATE messages SET status = 'queued', error = NULL, updated_at = ? WHERE id = ?",
                params![&now, id],
            )
            .await?;
            self.add_history(id, "queued", Some("retry requested"))
                .await?;
        }
        Ok(n == 1)
    }

    /// Crash recovery: resolve messages that were being processed when the
    /// daemon stopped. A part that was mid-transmission makes the whole
    /// message `submission_unknown`; nothing is re-sent automatically.
    pub async fn recover_processing(&self) -> DbResult<(u64, u64)> {
        let rows = self
            .rows(
                "SELECT message_id FROM outbound_queue WHERE status = 'processing'",
                vec![],
            )
            .await?;
        let (mut requeued, mut unknown) = (0, 0);
        for r in rows {
            let Some(id) = get_s(&r, "message_id") else {
                continue;
            };
            let sending = self
                .count(
                    "SELECT COUNT(*) FROM message_parts WHERE message_id = ? AND status = 'sending'",
                    params![&id],
                )
                .await?;
            if sending > 0 {
                self.exec(
                    "UPDATE message_parts SET status = 'unknown' WHERE message_id = ? AND status = 'sending'",
                    params![&id],
                )
                .await?;
                self.set_message_state(
                    &id,
                    "submission_unknown",
                    Some("daemon stopped while the modem was transmitting; not retried automatically"),
                    false,
                    None,
                )
                .await?;
                unknown += 1;
            } else {
                self.exec(
                    "UPDATE outbound_queue SET status = 'queued', claimed_by = NULL, updated_at = ? WHERE message_id = ?",
                    params![utils::now(), &id],
                )
                .await?;
                self.exec(
                    "UPDATE messages SET status = 'queued' WHERE id = ?",
                    params![&id],
                )
                .await?;
                self.add_history(&id, "queued", Some("recovered after restart"))
                    .await?;
                requeued += 1;
            }
        }
        Ok((requeued, unknown))
    }

    /// Expire queued messages whose deadline has passed. Returns their ids.
    pub async fn expire_due(&self) -> DbResult<Vec<String>> {
        let rows = self
            .rows(
                &format!("SELECT message_id FROM outbound_queue WHERE status IN ({CLAIMABLE}) AND expires_at IS NOT NULL AND expires_at < ?"),
                params![utils::now()],
            )
            .await?;
        let mut out = Vec::new();
        for r in rows {
            if let Some(id) = get_s(&r, "message_id") {
                self.set_message_state(&id, "expired", Some("not sent before expiry"), false, None)
                    .await?;
                out.push(id);
            }
        }
        Ok(out)
    }

    /// Outbound parts awaiting transmission for a message.
    pub async fn unsent_parts(&self, id: &str) -> DbResult<Vec<Part>> {
        let sql = format!(
            "SELECT {PART_COLS} FROM message_parts WHERE message_id = ? AND status IN ('queued', 'failed') ORDER BY sequence"
        );
        Ok(self
            .rows(&sql, params![id])
            .await?
            .iter()
            .map(part_from_row)
            .collect())
    }

    /// SMS segments submitted by a modem since `since` (quota accounting).
    pub async fn segments_submitted_since(&self, modem_id: &str, since: &str) -> DbResult<i64> {
        self.count(
            "SELECT COUNT(*) FROM message_parts WHERE direction = 'outbound' AND modem_id = ? AND submitted_at >= ?",
            params![modem_id, since],
        )
        .await
    }

    pub async fn last_submission_to(&self, recipient: &str) -> DbResult<Option<String>> {
        Ok(self
            .row(
                "SELECT MAX(submitted_at) AS t FROM messages WHERE direction = 'outbound' AND recipient = ?",
                params![recipient],
            )
            .await?
            .and_then(|r| get_s(&r, "t")))
    }

    /// Candidate outbound parts for a delivery report with this reference.
    pub async fn parts_by_modem_ref(
        &self,
        modem_id: &str,
        modem_ref: i64,
        since: &str,
    ) -> DbResult<Vec<Part>> {
        let sql = format!(
            "SELECT {PART_COLS} FROM message_parts WHERE direction = 'outbound' AND modem_id = ? AND modem_ref = ? AND status = 'submitted' AND submitted_at >= ? ORDER BY submitted_at DESC LIMIT 10"
        );
        Ok(self
            .rows(&sql, params![modem_id, modem_ref, since])
            .await?
            .iter()
            .map(part_from_row)
            .collect())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_delivery_report(
        &self,
        message_id: Option<&str>,
        part_id: Option<&str>,
        modem_id: &str,
        modem_ref: i64,
        recipient: &str,
        status: i64,
        category: &str,
        description: &str,
        smsc_timestamp: Option<String>,
        discharge_time: Option<String>,
        raw_pdu: Option<&str>,
        fingerprint: &str,
    ) -> DbResult<String> {
        let id = utils::new_id();
        self.exec(
            "INSERT INTO delivery_reports (id, message_id, part_id, modem_id, modem_ref, recipient, status, category, description, smsc_timestamp, discharge_time, raw_pdu, fingerprint, received_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                &id, message_id, part_id, modem_id, modem_ref, recipient, status, category,
                description, smsc_timestamp, discharge_time, raw_pdu, fingerprint, utils::now()
            ],
        )
        .await?;
        Ok(id)
    }

    pub async fn delivery_reports(&self, message_id: &str) -> DbResult<Vec<Json>> {
        self.json_rows(
            "SELECT id, part_id, modem_ref, recipient, status, category, description, smsc_timestamp, discharge_time, received_at FROM delivery_reports WHERE message_id = ? ORDER BY received_at",
            params![message_id],
        )
        .await
    }

    /// (total parts, delivered, failed) for an outbound message.
    pub async fn part_delivery_counts(&self, message_id: &str) -> DbResult<(i64, i64, i64)> {
        let total = self
            .count(
                "SELECT COUNT(*) FROM message_parts WHERE message_id = ?",
                params![message_id],
            )
            .await?;
        let delivered = self
            .count(
                "SELECT COUNT(*) FROM message_parts WHERE message_id = ? AND status = 'delivered'",
                params![message_id],
            )
            .await?;
        let failed = self
            .count(
                "SELECT COUNT(*) FROM message_parts WHERE message_id = ? AND status = 'delivery_failed'",
                params![message_id],
            )
            .await?;
        Ok((total, delivered, failed))
    }
}

impl Db {
    /// Remove stored content of a sent message (used when
    /// `sms.store_sent_messages` is disabled).
    pub async fn scrub_message_content(&self, id: &str) -> DbResult<()> {
        self.exec(
            "UPDATE messages SET body = NULL, payload = NULL, raw_pdu = NULL WHERE id = ?",
            params![id],
        )
        .await?;
        self.exec(
            "UPDATE message_parts SET pdu = NULL, payload = NULL WHERE message_id = ?",
            params![id],
        )
        .await?;
        Ok(())
    }
}
