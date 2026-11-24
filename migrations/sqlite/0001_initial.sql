-- THUGSred-smsGateway schema, migration 0001 (SQLite).
-- Timestamps are UTC strings in RFC 3339 format with millisecond precision.

CREATE TABLE messages (
    id TEXT PRIMARY KEY,
    direction TEXT NOT NULL,
    message_type TEXT NOT NULL,
    instance_id TEXT NOT NULL,
    modem_id TEXT,
    sim_id TEXT,
    sender TEXT,
    recipient TEXT,
    original_address TEXT,
    normalized_number TEXT,
    body TEXT,
    raw_pdu TEXT,
    payload BLOB,
    encoding TEXT,
    message_class INTEGER,
    protocol_id INTEGER,
    data_coding INTEGER,
    smsc TEXT,
    concat_ref INTEGER,
    concat_total INTEGER,
    parts INTEGER NOT NULL DEFAULT 1,
    src_port INTEGER,
    dst_port INTEGER,
    network_timestamp TEXT,
    received_at TEXT,
    submitted_at TEXT,
    delivered_at TEXT,
    status TEXT NOT NULL,
    delivery_status TEXT,
    error TEXT,
    retry_count INTEGER NOT NULL DEFAULT 0,
    metadata TEXT,
    integration_status TEXT,
    correlation_id TEXT,
    idempotency_key TEXT,
    fingerprint TEXT,
    modem_ref INTEGER,
    delivery_report INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX idx_messages_idempotency ON messages (idempotency_key);
CREATE UNIQUE INDEX idx_messages_fingerprint ON messages (fingerprint);
CREATE INDEX idx_messages_created ON messages (created_at);
CREATE INDEX idx_messages_direction ON messages (direction, created_at);
CREATE INDEX idx_messages_sender ON messages (sender);
CREATE INDEX idx_messages_recipient ON messages (recipient);
CREATE INDEX idx_messages_normalized ON messages (normalized_number);
CREATE INDEX idx_messages_status ON messages (status);
CREATE INDEX idx_messages_correlation ON messages (correlation_id);
CREATE INDEX idx_messages_modem_ref ON messages (modem_id, modem_ref);

CREATE TABLE message_parts (
    id TEXT PRIMARY KEY,
    message_id TEXT,
    direction TEXT NOT NULL,
    modem_id TEXT,
    address TEXT,
    concat_ref INTEGER,
    concat_total INTEGER,
    sequence INTEGER NOT NULL DEFAULT 1,
    wide_ref INTEGER NOT NULL DEFAULT 0,
    encoding TEXT,
    shifts INTEGER NOT NULL DEFAULT 0,
    pdu TEXT,
    payload BLOB,
    tpdu_length INTEGER,
    fingerprint TEXT,
    status TEXT NOT NULL,
    modem_ref INTEGER,
    error TEXT,
    submitted_at TEXT,
    delivered_at TEXT,
    created_at TEXT NOT NULL
);
CREATE UNIQUE INDEX idx_parts_fingerprint ON message_parts (fingerprint);
CREATE INDEX idx_parts_message ON message_parts (message_id, sequence);
CREATE INDEX idx_parts_pending ON message_parts (direction, status, address, concat_ref);
CREATE INDEX idx_parts_modem_ref ON message_parts (modem_id, modem_ref, status);

CREATE TABLE message_status_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id TEXT NOT NULL,
    status TEXT NOT NULL,
    detail TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_history_message ON message_status_history (message_id, id);

CREATE TABLE outbound_queue (
    message_id TEXT PRIMARY KEY,
    status TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 0,
    modem_id TEXT,
    claimed_by TEXT,
    not_before TEXT,
    expires_at TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TEXT,
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_queue_due ON outbound_queue (status, priority, created_at);

CREATE TABLE delivery_reports (
    id TEXT PRIMARY KEY,
    message_id TEXT,
    part_id TEXT,
    modem_id TEXT,
    modem_ref INTEGER NOT NULL,
    recipient TEXT,
    status INTEGER NOT NULL,
    category TEXT NOT NULL,
    description TEXT,
    smsc_timestamp TEXT,
    discharge_time TEXT,
    raw_pdu TEXT,
    fingerprint TEXT,
    received_at TEXT NOT NULL
);
CREATE UNIQUE INDEX idx_reports_fingerprint ON delivery_reports (fingerprint);
CREATE INDEX idx_reports_message ON delivery_reports (message_id);

CREATE TABLE events (
    id TEXT PRIMARY KEY,
    event_type TEXT NOT NULL,
    instance_id TEXT NOT NULL,
    correlation_id TEXT,
    data TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_events_created ON events (created_at);
CREATE INDEX idx_events_type ON events (event_type, created_at);

CREATE TABLE integration_events (
    id TEXT PRIMARY KEY,
    event_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    integration TEXT NOT NULL,
    status TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TEXT,
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    delivered_at TEXT
);
CREATE INDEX idx_deliveries_due ON integration_events (status, next_attempt_at);
CREATE INDEX idx_deliveries_integration ON integration_events (integration, created_at);
CREATE INDEX idx_deliveries_event ON integration_events (event_id);

CREATE TABLE integration_attempts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    delivery_id TEXT NOT NULL,
    integration TEXT NOT NULL,
    attempt INTEGER NOT NULL,
    success INTEGER NOT NULL,
    status_code INTEGER,
    error TEXT,
    duration_ms INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_attempts_delivery ON integration_attempts (delivery_id);
CREATE INDEX idx_attempts_created ON integration_attempts (created_at);

CREATE TABLE dead_letter_queue (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    reference_id TEXT,
    integration TEXT,
    event_id TEXT,
    reason TEXT,
    payload TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_dead_created ON dead_letter_queue (created_at);

CREATE TABLE calls (
    id TEXT PRIMARY KEY,
    modem_id TEXT,
    direction TEXT NOT NULL,
    number TEXT,
    state TEXT NOT NULL,
    answered INTEGER NOT NULL DEFAULT 0,
    missed INTEGER NOT NULL DEFAULT 0,
    started_at TEXT NOT NULL,
    answered_at TEXT,
    ended_at TEXT,
    duration_seconds INTEGER,
    termination_reason TEXT,
    recording_status TEXT,
    recording_id TEXT
);
CREATE INDEX idx_calls_started ON calls (started_at);

CREATE TABLE recordings (
    id TEXT PRIMARY KEY,
    call_id TEXT,
    path TEXT NOT NULL,
    format TEXT NOT NULL,
    duration_seconds INTEGER,
    size_bytes INTEGER,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_recordings_created ON recordings (created_at);

CREATE TABLE attachments (
    id TEXT PRIMARY KEY,
    message_id TEXT,
    filename TEXT NOT NULL,
    content_type TEXT,
    size_bytes INTEGER,
    path TEXT NOT NULL,
    sha256 TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_attachments_message ON attachments (message_id);

CREATE TABLE modem_health (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    modem_id TEXT NOT NULL,
    state TEXT NOT NULL,
    rssi_dbm INTEGER,
    signal_percent INTEGER,
    registration TEXT,
    operator TEXT,
    network_type TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_health_modem ON modem_health (modem_id, created_at);

CREATE TABLE sim_information (
    modem_id TEXT PRIMARY KEY,
    sim_id TEXT,
    iccid TEXT,
    imsi TEXT,
    msisdn TEXT,
    operator TEXT,
    pin_state TEXT,
    smsc TEXT,
    updated_at TEXT NOT NULL
);

CREATE TABLE ussd_sessions (
    id TEXT PRIMARY KEY,
    modem_id TEXT,
    code TEXT NOT NULL,
    response TEXT,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    completed_at TEXT
);
CREATE INDEX idx_ussd_created ON ussd_sessions (created_at);

CREATE TABLE gnss_fixes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    modem_id TEXT NOT NULL,
    latitude REAL NOT NULL,
    longitude REAL NOT NULL,
    altitude REAL,
    speed_kmh REAL,
    heading REAL,
    satellites INTEGER,
    fix_time TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_gnss_created ON gnss_fixes (created_at);

CREATE TABLE audit_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    actor TEXT,
    action TEXT NOT NULL,
    target TEXT,
    detail TEXT,
    request_id TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_audit_created ON audit_events (created_at);

CREATE TABLE automation_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    rule_name TEXT NOT NULL,
    sender TEXT,
    message_id TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_automation_rule ON automation_log (rule_name, created_at);

CREATE TABLE kv_state (
    k TEXT PRIMARY KEY,
    v TEXT,
    updated_at TEXT NOT NULL
);
