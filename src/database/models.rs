//! Typed records for the tables the gateway logic depends on.

use serde::Serialize;
use serde_json::Value as Json;

fn hex_opt<S: serde::Serializer>(v: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(b) => s.serialize_str(&hex::encode_upper(b)),
        None => s.serialize_none(),
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Message {
    pub id: String,
    /// inbound or outbound
    pub direction: String,
    /// sms, flash, binary, service, mms_notification, wap_push, unknown
    pub message_type: String,
    pub instance_id: String,
    pub modem_id: Option<String>,
    pub sim_id: Option<String>,
    pub sender: Option<String>,
    pub recipient: Option<String>,
    pub original_address: Option<String>,
    pub normalized_number: Option<String>,
    pub body: Option<String>,
    pub raw_pdu: Option<String>,
    #[serde(serialize_with = "hex_opt")]
    pub payload: Option<Vec<u8>>,
    pub encoding: Option<String>,
    pub message_class: Option<i64>,
    pub protocol_id: Option<i64>,
    pub data_coding: Option<i64>,
    pub smsc: Option<String>,
    pub concat_ref: Option<i64>,
    pub concat_total: Option<i64>,
    pub parts: i64,
    pub src_port: Option<i64>,
    pub dst_port: Option<i64>,
    pub network_timestamp: Option<String>,
    pub received_at: Option<String>,
    pub submitted_at: Option<String>,
    pub delivered_at: Option<String>,
    pub status: String,
    pub delivery_status: Option<String>,
    pub error: Option<String>,
    pub retry_count: i64,
    pub metadata: Option<Json>,
    pub integration_status: Option<String>,
    pub correlation_id: Option<String>,
    pub idempotency_key: Option<String>,
    #[serde(skip)]
    pub fingerprint: Option<String>,
    pub modem_ref: Option<i64>,
    pub delivery_report: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Part {
    pub id: String,
    pub message_id: Option<String>,
    pub direction: String,
    pub modem_id: Option<String>,
    /// Sender (inbound) or recipient (outbound).
    pub address: Option<String>,
    pub concat_ref: Option<i64>,
    pub concat_total: Option<i64>,
    pub sequence: i64,
    pub wide_ref: bool,
    pub encoding: Option<String>,
    /// National language shift tables: single * 256 + locking.
    pub shifts: i64,
    pub pdu: Option<String>,
    #[serde(serialize_with = "hex_opt")]
    pub payload: Option<Vec<u8>>,
    pub tpdu_length: Option<i64>,
    #[serde(skip)]
    pub fingerprint: Option<String>,
    /// pending, assembled, queued, sending, submitted, delivered, failed, unknown
    pub status: String,
    pub modem_ref: Option<i64>,
    pub error: Option<String>,
    pub submitted_at: Option<String>,
    pub delivered_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct QueueItem {
    pub message_id: String,
    pub status: String,
    pub priority: i64,
    pub modem_id: Option<String>,
    pub claimed_by: Option<String>,
    pub not_before: Option<String>,
    pub expires_at: Option<String>,
    pub attempts: i64,
    pub next_attempt_at: Option<String>,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Search parameters for message history.
#[derive(Debug, Clone, Default)]
pub struct MessageQuery {
    pub direction: Option<String>,
    pub message_type: Option<String>,
    pub status: Option<String>,
    pub sender: Option<String>,
    pub recipient: Option<String>,
    pub modem_id: Option<String>,
    /// Free text matched against body, sender and recipient.
    pub search: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub limit: i64,
    pub offset: i64,
    pub ascending: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Delivery {
    pub id: String,
    pub event_id: String,
    pub event_type: String,
    pub integration: String,
    pub status: String,
    pub attempts: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventRecord {
    pub id: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub instance_id: String,
    pub correlation_id: Option<String>,
    pub timestamp: String,
    pub data: Json,
}
